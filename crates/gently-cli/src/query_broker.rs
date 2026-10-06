//! Read-only delegation to the already-unlocked watcher. The token stays in
//! its memory; no TCP listener, secret handoff, vault calls or native auth reads.

use crate::config::Config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const MAX_REQUEST: u64 = 64 * 1024;
const MAX_RESPONSE: u64 = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    collector_url: String,
    tenant_id: String,
    params: Vec<(String, String)>,
}

/// Validate without following a symlink. A socket and its directory must be
/// private and owned by the same user. Same-user processes share query access.
pub fn validate_socket(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    let parent = std::fs::symlink_metadata(path.parent().context("socket has no parent")?)?;
    anyhow::ensure!(
        metadata.file_type().is_socket()
            && metadata.mode() & 0o777 == 0o600
            && metadata.uid() == unsafe { libc::geteuid() },
        "query socket must be an owner-only Unix socket"
    );
    anyhow::ensure!(
        parent.is_dir() && parent.mode() & 0o777 == 0o700 && metadata.uid() == parent.uid(),
        "query socket directory must be owner-only"
    );
    Ok(())
}

pub struct QueryBroker {
    listener: std::os::unix::net::UnixListener,
    path: PathBuf,
    inode: u64,
}

impl QueryBroker {
    /// Caller holds the exporter lock, serializing stale-socket recovery.
    pub fn bind(cfg: &Config) -> Result<Self> {
        cfg.require_collector()?;
        cfg.ensure_state_dir()?;
        let path = cfg.runtime_dir().join("query.sock");
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                anyhow::ensure!(
                    metadata.file_type().is_socket(),
                    "query socket path is not a socket"
                );
                validate_socket(&path)?;
                match std::os::unix::net::UnixStream::connect(&path) {
                    Ok(_) => anyhow::bail!("query socket is already in use"),
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                        std::fs::remove_file(&path)?
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let inode = std::fs::symlink_metadata(&path)?.ino();
        Ok(Self {
            listener,
            path,
            inode,
        })
    }

    pub async fn serve(&self, cfg: &Config) -> Result<()> {
        let listener = UnixListener::from_std(self.listener.try_clone()?)?;
        let timeout = Duration::from_secs(cfg.query_timeout_secs.max(1));
        let client = std::sync::Arc::new(crate::collector::CollectorClient::new(
            &cfg.collector_url,
            &cfg.token,
            &cfg.tenant_id,
            cfg.query_timeout_secs.max(1),
        )?);
        let mut requests = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = accept_with_retry(|| listener.accept()) => {
                    let (mut stream, _) = accepted;
                    // ACLs can grant access beyond BSD mode bits. Authenticate
                    // each socket peer before delegating the watcher credential.
                    if !stream.peer_cred().is_ok_and(|peer| peer.uid() == unsafe { libc::geteuid() }) { continue; }
                    if requests.len() >= 16 {
                        let _ = tokio::time::timeout(Duration::from_millis(100),
                            stream.write_all(b"{\"error\":\"query broker busy\"}\n")).await;
                        continue;
                    }
                    let client = client.clone();
                    let url = cfg.collector_url.trim_end_matches('/').to_string();
                    let tenant = cfg.tenant_id.clone();
                    requests.spawn(async move {
                        let _ = tokio::time::timeout(timeout, respond(stream, &client, &url, &tenant)).await;
                    });
                }
                _ = requests.join_next(), if !requests.is_empty() => {}
            }
        }
    }
}

async fn accept_with_retry<T, F, Fut>(mut accept: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>>,
{
    let mut delay = Duration::from_millis(25);
    loop {
        match accept().await {
            Ok(value) => return value,
            Err(_) => {
                // Connection aborts and resource pressure must not stop export.
                // Never log platform error text or any client-provided value.
                tracing::warn!("local query broker accept failed; retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(1));
            }
        }
    }
}

impl Drop for QueryBroker {
    fn drop(&mut self) {
        // Leave a replaced path untouched.
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.file_type().is_socket() && m.ino() == self.inode)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

async fn read_line(stream: impl tokio::io::AsyncRead + Unpin, cap: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    BufReader::new(stream.take(cap + 1))
        .read_until(b'\n', &mut bytes)
        .await?;
    anyhow::ensure!(
        bytes.len() as u64 <= cap && bytes.last() == Some(&b'\n'),
        "invalid query message size or framing"
    );
    Ok(bytes)
}

async fn respond(
    mut stream: UnixStream,
    client: &crate::collector::CollectorClient,
    url: &str,
    tenant: &str,
) -> Result<()> {
    let response = match forward(&mut stream, client, url, tenant).await {
        Ok(value) => json!({"result": value}),
        // Never forward remote error bodies, request parameters or credentials.
        Err(e) => json!({"error": e.to_string()}),
    };
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    Ok(())
}

async fn forward(
    stream: &mut UnixStream,
    client: &crate::collector::CollectorClient,
    url: &str,
    tenant: &str,
) -> Result<Value> {
    let request: Request = serde_json::from_slice(&read_line(stream, MAX_REQUEST).await?)
        .map_err(|_| anyhow::anyhow!("invalid query request"))?;
    anyhow::ensure!(
        request.collector_url.trim_end_matches('/') == url,
        "query collector differs from watcher's collector"
    );
    anyhow::ensure!(
        request.tenant_id == tenant,
        "query tenant differs from watcher tenant"
    );
    if request
        .params
        .iter()
        .any(|(key, value)| key == "op" && value == "raw")
    {
        anyhow::ensure!(
            request.params.len() == 2
                && request
                    .params
                    .iter()
                    .filter(|(key, value)| key == "op" && value == "raw")
                    .count()
                    == 1,
            "unsupported ciphertext query parameters"
        );
        let reference = request
            .params
            .iter()
            .find(|(key, _)| key == "raw_ref")
            .context("ciphertext query requires raw_ref")?;
        return serde_json::to_value(client.fetch_raw(&reference.1).await?)
            .context("invalid ciphertext response");
    }
    let ops: Vec<_> = request
        .params
        .iter()
        .filter(|(key, _)| key == "op")
        .collect();
    anyhow::ensure!(
        ops.len() == 1 && matches!(ops[0].1.as_str(), "traces" | "trace" | "spans" | "stats"),
        "query operation is not read-only"
    );
    for (key, _) in &request.params {
        anyhow::ensure!(
            matches!(
                key.as_str(),
                "op" | "trace_id"
                    | "session_id"
                    | "harness"
                    | "tool_name"
                    | "name"
                    | "status"
                    | "kind"
                    | "since"
                    | "until"
                    | "limit"
                    | "order"
                    | "page"
                    | "cursor"
            ),
            "unsupported query parameter"
        );
    }
    let params: Vec<_> = request
        .params
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    client.query(&params).await
}

pub async fn query<T: for<'de> Deserialize<'de>>(
    path: &Path,
    base: &str,
    tenant: &str,
    params: &[(&str, String)],
    timeout: Duration,
) -> Result<T> {
    validate_socket(path)?;
    tokio::time::timeout(timeout, async {
        let mut stream = UnixStream::connect(path)
            .await
            .context("local query watcher is unavailable")?;
        anyhow::ensure!(
            stream
                .peer_cred()
                .context("cannot verify local query watcher")?
                .uid()
                == unsafe { libc::geteuid() },
            "local query watcher belongs to another user"
        );
        let request = Request {
            collector_url: base.strip_suffix("/v1/query").unwrap_or(base).into(),
            tenant_id: tenant.into(),
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        };
        let mut bytes = serde_json::to_vec(&request)?;
        anyhow::ensure!(
            (bytes.len() as u64) < MAX_REQUEST,
            "query request too large"
        );
        bytes.push(b'\n');
        stream.write_all(&bytes).await?;
        let response =
            crate::json_fidelity::parse_bytes(&read_line(&mut stream, MAX_RESPONSE).await?)?;
        if let Some(error) = response.get("error").and_then(Value::as_str) {
            anyhow::bail!("{error}");
        }
        serde_json::from_value(
            response
                .get("result")
                .context("query response missing result")?
                .clone(),
        )
        .map_err(|_| anyhow::anyhow!("invalid local query response"))
    })
    .await
    .context("local query watcher timed out")?
}

#[cfg(test)]
mod retry_tests {
    #[tokio::test]
    async fn transient_accept_errors_do_not_stop_the_broker() {
        let mut calls = 0;
        let accepted = super::accept_with_retry(|| {
            calls += 1;
            std::future::ready(match calls {
                1 => Err(std::io::Error::from(std::io::ErrorKind::Interrupted)),
                2 => Err(std::io::Error::from(std::io::ErrorKind::ConnectionAborted)),
                3 => Err(std::io::Error::from_raw_os_error(libc::EMFILE)),
                _ => Ok("accepted synthetic connection"),
            })
        })
        .await;
        assert_eq!(accepted, "accepted synthetic connection");
        assert_eq!(calls, 4);
    }
}
