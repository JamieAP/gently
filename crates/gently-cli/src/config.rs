//! Configuration resolution: `~/.gently/config.toml` overlaid by environment.
//!
//! The hook path needs only `state_dir`; the exporter and query/MCP paths also
//! need `collector_url` + `token`. Those are validated lazily ([`Config::require_collector`])
//! so a hook never fails just because the collector is unconfigured.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

/// Resolved runtime configuration.
#[derive(Clone, Debug)]
pub struct Config {
    pub collector_url: String,
    pub token: String,
    pub state_dir: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    collector_url: String,
    #[serde(default)]
    token: String,
}

impl Config {
    /// Load config from `<state_dir>/config.toml`, then apply env overrides.
    /// `state_dir` is `$GENTLY_STATE_DIR` or `~/.gently`.
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

        let collector_url = env_or("GENTLY_COLLECTOR_URL", file.collector_url);
        let token = env_or("GENTLY_TOKEN", file.token);

        Ok(Self {
            collector_url,
            token,
            state_dir,
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
