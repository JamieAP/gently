//! HTTP/3 (QUIC) transport - lightweight, zero added Rust dependencies.
//!
//! A pure-Rust QUIC stack (quinn/h3) would add dozens of crates and minutes of
//! compile time for one POST path. Instead this transport delegates the QUIC
//! stack to `curl --http3-only`, which is detected at runtime: the binary stays
//! light and gains genuine HTTP/3 only where a capable `curl` exists. When no
//! HTTP/3-capable curl is present, [`detect`](QuicTransport::detect) returns
//! `None` and the caller falls back to HTTP/2 - see [`PreferQuic`](crate::PreferQuic).
//!
//! The bearer token is written into a private (0600) curl config file rather
//! than passed on the command line, so it never appears in the process table.

use crate::{ExportError, Transport};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// QUIC transport backed by an HTTP/3-capable `curl`.
pub struct QuicTransport {
    curl: PathBuf,
    config_path: PathBuf,
}

impl QuicTransport {
    /// Detect an HTTP/3-capable `curl` and prepare a private config file under
    /// `state_dir`. Returns `None` when no such curl exists, so the caller can
    /// fall back to HTTP/2 without QUIC support being fatal.
    pub fn detect(collector_url: &str, token: &str, state_dir: &Path) -> Option<Self> {
        let curl = find_http3_curl()?;
        let endpoint = format!("{}/v1/traces", collector_url.trim_end_matches('/'));
        let config_path = state_dir.join(".curl-h3.cfg");
        if write_config(&config_path, &endpoint, token).is_err() {
            return None;
        }
        Some(Self { curl, config_path })
    }
}

impl Transport for QuicTransport {
    async fn send(&self, body: Vec<u8>) -> Result<(), ExportError> {
        // curl is blocking; run it off the async runtime. This keeps the export
        // crate on the tokio feature set the CLI already compiles (no `process`
        // feature, hence no tokio recompile).
        let curl = self.curl.clone();
        let config = self.config_path.clone();
        tokio::task::spawn_blocking(move || run_curl(&curl, &config, &body))
            .await
            .map_err(|e| ExportError::Transport(format!("curl task join: {e}")))?
    }
}

fn run_curl(curl: &Path, config: &Path, body: &[u8]) -> Result<(), ExportError> {
    let mut child = std::process::Command::new(curl)
        .arg("--config")
        .arg(config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| ExportError::Transport(format!("spawning curl: {e}")))?;

    // Feed the OTLP body on stdin (curl config uses `data-binary = "@-"`); the
    // scoped take() drops the pipe, signalling EOF before we await the exit.
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ExportError::Transport("curl stdin unavailable".into()))?;
        stdin
            .write_all(body)
            .map_err(|e| ExportError::Transport(format!("writing curl stdin: {e}")))?;
    }

    let out = child
        .wait_with_output()
        .map_err(|e| ExportError::Transport(format!("awaiting curl: {e}")))?;
    if !out.status.success() {
        // curl exit 7 = connect failed, 99 = http3 unsupported by peer, etc.
        return Err(ExportError::Transport(format!(
            "curl --http3-only failed (exit {:?})",
            out.status.code()
        )));
    }

    // The config writes `%{http_code}` to stdout.
    let code: u16 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| ExportError::Transport("curl produced no status code".into()))?;
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(ExportError::Transport(format!("collector returned {code} over http3")))
    }
}

/// Candidate curl paths, checked in order for an `HTTP3` feature.
fn find_http3_curl() -> Option<PathBuf> {
    const CANDIDATES: &[&str] = &[
        "curl",
        "/opt/homebrew/opt/curl/bin/curl",
        "/usr/local/opt/curl/bin/curl",
        "/opt/homebrew/bin/curl",
    ];
    CANDIDATES.iter().map(PathBuf::from).find(|p| curl_has_http3(p))
}

fn curl_has_http3(curl: &Path) -> bool {
    std::process::Command::new(curl)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("HTTP3"))
        .unwrap_or(false)
}

/// Write a 0600 curl config file carrying the endpoint, headers (incl. the
/// bearer token), and HTTP/3-only mode. Keeping the token here rather than in
/// argv means it never shows up in `ps`.
fn write_config(path: &Path, endpoint: &str, token: &str) -> std::io::Result<()> {
    let config = format!(
        "url = \"{endpoint}\"\n\
         request = \"POST\"\n\
         http3-only\n\
         header = \"authorization: Bearer {token}\"\n\
         header = \"content-type: application/json\"\n\
         data-binary = \"@-\"\n\
         silent\n\
         show-error\n\
         output = \"/dev/null\"\n\
         write-out = \"%{{http_code}}\"\n"
    );
    std::fs::write(path, config)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
