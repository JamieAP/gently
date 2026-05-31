//! Configuration resolution: `~/.gently/config.toml` overlaid by environment.
//!
//! Two essentials - `collector_url` + `token` - point at the collector (a
//! Cloudflare Worker backed by D1, or a local `wrangler dev`); both the exporter
//! and the query/MCP paths use them. The rest are operational tunables with sane
//! defaults, so a minimal config (just url + token) Just Works. The hook path
//! needs only `state_dir`, so its requirements are validated lazily
//! ([`Config::require_collector`]) and a hook never fails for lack of a collector.

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
#[derive(Clone, Debug)]
pub struct Config {
    /// Collector base URL (the Worker; `/v1/traces` and `/v1/query` are appended).
    pub collector_url: String,
    /// Shared bearer token for the collector.
    pub token: String,
    /// Local state directory (outbox db, logs, raw captures).
    pub state_dir: PathBuf,
    /// Prefer HTTP/3 (QUIC) for export, falling back to HTTP/2.
    pub prefer_quic: bool,
    /// Max buffered outbox rows before the oldest are dropped.
    pub outbox_cap: usize,
    /// Spans coalesced into one export request.
    pub export_batch: usize,
    /// Per-request export timeout.
    pub export_timeout_secs: u64,
    /// Per-request query/MCP timeout.
    pub query_timeout_secs: u64,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    collector_url: String,
    #[serde(default)]
    token: String,
    prefer_quic: Option<bool>,
    outbox_cap: Option<usize>,
    export_batch: Option<usize>,
    export_timeout_secs: Option<u64>,
    query_timeout_secs: Option<u64>,
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

        let file: FileConfig = {
            let path = state_dir.join("config.toml");
            match std::fs::read_to_string(&path) {
                Ok(s) => {
                    toml::from_str(&s).with_context(|| format!("parsing {}", path.display()))?
                }
                Err(_) => FileConfig::default(),
            }
        };

        Ok(Self {
            collector_url: env_or("GENTLY_COLLECTOR_URL", file.collector_url),
            token: env_or("GENTLY_TOKEN", file.token),
            state_dir,
            prefer_quic: file.prefer_quic.unwrap_or(DEFAULT_PREFER_QUIC),
            outbox_cap: file.outbox_cap.unwrap_or(gently_store::OUTBOX_CAP),
            export_batch: file.export_batch.unwrap_or(DEFAULT_EXPORT_BATCH),
            export_timeout_secs: file
                .export_timeout_secs
                .unwrap_or(DEFAULT_EXPORT_TIMEOUT_SECS),
            query_timeout_secs: file
                .query_timeout_secs
                .unwrap_or(DEFAULT_QUERY_TIMEOUT_SECS),
        })
    }

    /// Path to the local state database.
    pub fn state_db(&self) -> PathBuf {
        self.state_dir.join("state.db")
    }

    /// Ensure the state directory exists.
    pub fn ensure_state_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.state_dir)
            .with_context(|| format!("creating {}", self.state_dir.display()))?;
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
            "token is not configured (set GENTLY_TOKEN or config.toml)"
        );
        Ok(())
    }
}

fn env_or(key: &str, fallback: String) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "collector_url = \"https://x.workers.dev\"\ntoken = \"t\"\n",
        )
        .unwrap();
        // GENTLY_STATE_DIR is process-global; set it just for this resolution.
        std::env::set_var("GENTLY_STATE_DIR", dir.path());
        let cfg = Config::load().unwrap();
        std::env::remove_var("GENTLY_STATE_DIR");

        assert_eq!(cfg.collector_url, "https://x.workers.dev");
        assert!(cfg.prefer_quic);
        assert_eq!(cfg.outbox_cap, gently_store::OUTBOX_CAP);
        assert_eq!(cfg.export_batch, DEFAULT_EXPORT_BATCH);
        assert_eq!(cfg.export_timeout_secs, DEFAULT_EXPORT_TIMEOUT_SECS);
        assert_eq!(cfg.query_timeout_secs, DEFAULT_QUERY_TIMEOUT_SECS);
    }
}
