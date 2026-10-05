//! Configuration resolution: `~/.gently/config.toml` overlaid by environment.
//!
//! Collector location and tenant/device preferences live in config.toml.
//! Authentication is inherited through GENTLY_TOKEN from a secret provider;
//! configuration never serializes that credential. The hook remains usable
//! offline and captures metadata without a collector token.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

/// Default operational tunables. `outbox_cap`'s default is the store's own
/// constant so there's a single source of truth for it.
const DEFAULT_PREFER_QUIC: bool = true;
const DEFAULT_EXPORT_BATCH: usize = 512;
const DEFAULT_EXPORT_TIMEOUT_SECS: u64 = 15;
const DEFAULT_QUERY_TIMEOUT_SECS: u64 = 30;

/// Resolved runtime configuration.
#[derive(Clone)]
pub struct Config {
    /// Collector base URL (the Worker; `/v1/traces` and `/v1/query` are appended).
    pub collector_url: String,
    /// Inherited per-host bearer token for the collector.
    pub token: String,
    /// Local state directory (outbox db, logs, raw captures).
    pub state_dir: PathBuf,
    /// Prefer HTTP/3 (QUIC) for export, falling back to HTTP/2.
    pub prefer_quic: bool,
    /// Max buffered outbox rows before the oldest are dropped.
    pub outbox_cap: usize,
    /// OTLP envelope rows coalesced into one export request.
    pub export_batch: usize,
    /// Per-request export timeout.
    pub export_timeout_secs: u64,
    /// Per-request query/MCP timeout.
    pub query_timeout_secs: u64,
    /// Retain full prompt/tool values locally; disabled unless explicitly enabled.
    pub capture_raw_values: bool,
    pub tenant_id: String,
    pub device_id: String,
    pub sync_raw_values: bool,
    pub resolve_raw_values: bool,
    pub raw_manifest: Option<PathBuf>,
    pub raw_trust: Option<PathBuf>,
    pub raw_identity: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    collector_url: String,
    prefer_quic: Option<bool>,
    outbox_cap: Option<usize>,
    export_batch: Option<usize>,
    export_timeout_secs: Option<u64>,
    query_timeout_secs: Option<u64>,
    capture_raw_values: Option<bool>,
    tenant_id: Option<String>,
    device_id: Option<String>,
    sync_raw_values: Option<bool>,
    resolve_raw_values: Option<bool>,
    raw_manifest: Option<PathBuf>,
    raw_trust: Option<PathBuf>,
    raw_identity: Option<PathBuf>,
}

impl Config {
    /// Load config from `<state_dir>/config.toml`, then apply env overrides for
    /// the essentials. `state_dir` is `$GENTLY_STATE_DIR` or `~/.gently`.
    pub fn load() -> Result<Self> {
        let state_dir = match std::env::var_os("GENTLY_STATE_DIR") {
            Some(d) => PathBuf::from(d),
            None => dirs::home_dir()
                .context("cannot determine home directory")?
                .join(".gently"),
        };

        gently_store::private_fs::ensure_private_dir(&state_dir)?;
        let file: FileConfig = {
            let path = state_dir.join("config.toml");
            gently_store::private_fs::harden_existing_file(&path)?;
            match std::fs::read_to_string(&path) {
                Ok(s) => {
                    toml::from_str(&s).map_err(|_| anyhow::anyhow!("invalid configuration; check documented fields and values (credentials must be inherited)"))?
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileConfig::default(),
                Err(_) => anyhow::bail!("cannot read Gently configuration"),
            }
        };

        let capture_raw_values = env_bool("GENTLY_CAPTURE_RAW_VALUES", file.capture_raw_values)?;
        Ok(Self {
            collector_url: env_or("GENTLY_COLLECTOR_URL", file.collector_url),
            token: std::env::var("GENTLY_TOKEN").unwrap_or_default(),
            state_dir,
            capture_raw_values,
            tenant_id: env_or(
                "GENTLY_TENANT_ID",
                file.tenant_id.unwrap_or_else(|| "personal".into()),
            ),
            device_id: env_or(
                "GENTLY_DEVICE_ID",
                file.device_id.unwrap_or_else(|| "local".into()),
            ),
            sync_raw_values: env_bool("GENTLY_SYNC_RAW_VALUES", file.sync_raw_values)?,
            resolve_raw_values: env_bool("GENTLY_RESOLVE_RAW_VALUES", file.resolve_raw_values)?,
            raw_manifest: env_path("GENTLY_RAW_MANIFEST", file.raw_manifest),
            raw_trust: env_path("GENTLY_RAW_TRUST", file.raw_trust),
            raw_identity: env_path("GENTLY_RAW_IDENTITY", file.raw_identity),
            prefer_quic: file.prefer_quic.unwrap_or(DEFAULT_PREFER_QUIC),
            outbox_cap: file.outbox_cap.unwrap_or(gently_store::OUTBOX_CAP),
            export_batch: file.export_batch.unwrap_or(DEFAULT_EXPORT_BATCH),
            export_timeout_secs: file
                .export_timeout_secs
                .unwrap_or(DEFAULT_EXPORT_TIMEOUT_SECS),
            // Env-overridable so a latency-sensitive caller (e.g. a tmux launcher
            // resolving a pane) can demand a tight fast-fail instead of the 30s
            // default that suits interactive querying.
            query_timeout_secs: env_u64("GENTLY_QUERY_TIMEOUT_SECS")
                .or(file.query_timeout_secs)
                .unwrap_or(DEFAULT_QUERY_TIMEOUT_SECS),
        })
    }

    /// Path to the local state database.
    pub fn state_db(&self) -> PathBuf {
        self.runtime_dir().join("state.db")
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.state_dir
            .join("tenants")
            .join(&self.tenant_id)
            .join("devices")
            .join(&self.device_id)
    }

    /// Ensure the state directory exists.
    pub fn ensure_state_dir(&self) -> Result<()> {
        anyhow::ensure!(
            valid_id(&self.tenant_id) && valid_id(&self.device_id),
            "invalid tenant or device identifier"
        );
        gently_store::private_fs::ensure_private_dir(&self.state_dir)
            .with_context(|| format!("creating {}", self.state_dir.display()))?;
        let tenants = self.state_dir.join("tenants");
        let tenant = tenants.join(&self.tenant_id);
        let devices = tenant.join("devices");
        for path in [&tenants, &tenant, &devices, &self.runtime_dir()] {
            gently_store::private_fs::ensure_private_dir(path)?;
        }
        Ok(())
    }

    /// Validate that a collector URL + token are configured (for export/query).
    pub fn require_collector(&self) -> Result<()> {
        anyhow::ensure!(
            !self.collector_url.is_empty(),
            "collector_url is not configured (set GENTLY_COLLECTOR_URL or config.toml)"
        );
        anyhow::ensure!(
            !self.token.is_empty(),
            "token is not configured (supply GENTLY_TOKEN through your secret provider)"
        );
        anyhow::ensure!(
            valid_id(&self.tenant_id) && valid_id(&self.device_id),
            "tenant_id and device_id must be 1–64 ASCII letters, digits, underscores or hyphens"
        );
        let url = reqwest::Url::parse(&self.collector_url)
            .map_err(|_| anyhow::anyhow!("invalid collector URL"))?;
        let loopback = url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        anyhow::ensure!((url.scheme() == "https" || (url.scheme() == "http" && loopback)) && url.host_str().is_some() && url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none(), "collector URL must use HTTPS or loopback HTTP and contain no credentials, query or fragment");
        Ok(())
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn env_bool(key: &str, fallback: Option<bool>) -> Result<bool> {
    match std::env::var(key).as_deref() {
        Ok("1" | "true") => Ok(true),
        Ok("0" | "false") => Ok(false),
        Ok(_) => anyhow::bail!("{key} must be 1, 0, true or false"),
        Err(_) => Ok(fallback.unwrap_or(false)),
    }
}

fn env_path(key: &str, fallback: Option<PathBuf>) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or(fallback)
}

fn env_or(key: &str, fallback: String) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or(fallback)
}

/// Read a `u64` from an env var, or `None` if unset/empty/unparseable.
fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_collector_tokens_are_rejected() {
        assert!(toml::from_str::<FileConfig>("token = 'synthetic-fixture'").is_err());
    }

    #[test]
    fn defaults_apply_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "collector_url = \"https://x.workers.dev\"\n",
        )
        .unwrap();
        // GENTLY_STATE_DIR is process-global; set it just for this resolution.
        std::env::set_var("GENTLY_STATE_DIR", dir.path());
        let cfg = Config::load().unwrap();
        std::env::remove_var("GENTLY_STATE_DIR");

        assert_eq!(cfg.collector_url, "https://x.workers.dev");
        assert!(cfg.prefer_quic);
        assert!(!cfg.capture_raw_values);
        assert!(!cfg.sync_raw_values);
        assert!(!cfg.resolve_raw_values);
        assert_eq!(cfg.outbox_cap, gently_store::OUTBOX_CAP);
        assert_eq!(cfg.export_batch, DEFAULT_EXPORT_BATCH);
        assert_eq!(cfg.export_timeout_secs, DEFAULT_EXPORT_TIMEOUT_SECS);
        assert_eq!(cfg.query_timeout_secs, DEFAULT_QUERY_TIMEOUT_SECS);
    }
}
