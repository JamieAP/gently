//! Configuration resolution: `~/.gently/config.toml` overlaid by environment.
//!
//! Collector location and tenant/device preferences live in config.toml.
//! Authentication is inherited through GENTLY_TOKEN from a secret provider;
//! configuration never serializes that credential. The hook remains usable
//! offline and captures metadata without a collector token.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Default operational tunables. `outbox_cap`'s default is the store's own
/// constant so there's a single source of truth for it.
const DEFAULT_PREFER_QUIC: bool = true;
const DEFAULT_EXPORT_BATCH: usize = 512;
const DEFAULT_EXPORT_TIMEOUT_SECS: u64 = 15;
const DEFAULT_QUERY_TIMEOUT_SECS: u64 = 30;

pub fn check_setup() -> Result<()> {
    let cfg = Config::load()?;
    if cfg.capture_raw_values {
        let manifest_path = cfg
            .raw_manifest
            .as_deref()
            .context("capture_raw_values requires raw_manifest")?;
        let trust_path = cfg
            .raw_trust
            .as_deref()
            .context("capture_raw_values requires raw_trust")?;
        let manifest: gently_raw::SignedManifest = read_policy_json(manifest_path)?;
        let trust: gently_raw::TrustPin = read_policy_json(trust_path)?;
        let verified = gently_raw::VerifiedManifest::verify(&manifest, &trust).map_err(|_| {
            anyhow::anyhow!("recipient policy or trust pin is invalid, expired or rolled back")
        })?;
        anyhow::ensure!(
            verified.manifest().tenant_id == cfg.tenant_id,
            "recipient policy tenant does not match tenant_id"
        );
    }
    if cfg.resolve_raw_values {
        let identity = cfg
            .raw_identity
            .as_deref()
            .context("resolve_raw_values requires raw_identity")?;
        // Inspect only the protected file's metadata. Setup checks never parse
        // a private identity, invoke a plugin, or request a passphrase.
        gently_store::private_fs::harden_existing_file(identity)
            .context("raw_identity must be a safe regular file owned by this user")?;
        let metadata = std::fs::symlink_metadata(identity)
            .context("raw_identity file does not exist or cannot be inspected")?;
        anyhow::ensure!(metadata.is_file(), "raw_identity must be a regular file");
    }
    cfg.ensure_state_dir()?;
    let database = cfg.state_db();
    if std::fs::symlink_metadata(&database).is_ok() {
        gently_store::Store::open(&database).context(
            "local state schema check failed; explicitly move or reset disposable application state",
        )?;
    }
    eprintln!("gently: local setup checks passed");
    Ok(())
}

fn read_policy_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<T> {
    use std::io::Read;
    const MAX_POLICY_BYTES: u64 = 64 * 1024;
    gently_store::private_fs::harden_existing_file(path)
        .context("public recipient policy path is unsafe or unreadable")?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .context("public recipient policy file is missing or unreadable")?
        .take(MAX_POLICY_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("cannot read public recipient policy")?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_POLICY_BYTES,
        "public recipient policy exceeds 64 KiB"
    );
    serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid public recipient policy JSON"))
}

pub fn print_setup_json() -> Result<()> {
    let cfg = Config::load()?;
    // This allowlisted view is deliberately separate from Config: adding a
    // credential or reader field to Config cannot accidentally serialize it.
    #[derive(Serialize)]
    struct PublicSetup<'a> {
        collector_url: &'a str,
        state_dir: &'a std::path::Path,
        tenant_id: &'a str,
        device_id: &'a str,
    }
    let view = PublicSetup {
        collector_url: &cfg.collector_url,
        state_dir: &cfg.state_dir,
        tenant_id: &cfg.tenant_id,
        device_id: &cfg.device_id,
    };
    println!("{}", serde_json::to_string(&view)?);
    Ok(())
}

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

        let state_dir = if state_dir.is_absolute() {
            state_dir
        } else {
            std::env::current_dir()
                .context("cannot resolve relative GENTLY_STATE_DIR")?
                .join(state_dir)
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
        let cfg = Self {
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
            query_timeout_secs: env_u64("GENTLY_QUERY_TIMEOUT_SECS")?
                .or(file.query_timeout_secs)
                .unwrap_or(DEFAULT_QUERY_TIMEOUT_SECS),
        };
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            valid_id(&self.tenant_id) && valid_id(&self.device_id),
            "tenant_id and device_id must be 1–64 ASCII letters, digits, underscores or hyphens"
        );
        anyhow::ensure!(
            (1..=1_000_000).contains(&self.outbox_cap),
            "outbox_cap must be between 1 and 1000000"
        );
        anyhow::ensure!(
            (1..=4096).contains(&self.export_batch),
            "export_batch must be between 1 and 4096"
        );
        anyhow::ensure!(
            (1..=3600).contains(&self.export_timeout_secs),
            "export_timeout_secs must be between 1 and 3600"
        );
        anyhow::ensure!(
            (1..=3600).contains(&self.query_timeout_secs),
            "query_timeout_secs (or GENTLY_QUERY_TIMEOUT_SECS) must be between 1 and 3600"
        );
        for (name, path) in [
            ("raw_manifest", self.raw_manifest.as_deref()),
            ("raw_trust", self.raw_trust.as_deref()),
            ("raw_identity", self.raw_identity.as_deref()),
        ] {
            anyhow::ensure!(
                path.is_none_or(std::path::Path::is_absolute),
                "{name} must use an absolute path (expand ~ in your shell)"
            );
        }
        if !self.collector_url.is_empty() {
            validate_collector_url(&self.collector_url)?;
        }
        Ok(())
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

    /// Queries need the public collector location even when using a local broker.
    pub fn require_collector_url(&self) -> Result<()> {
        anyhow::ensure!(
            !self.collector_url.is_empty(),
            "collector_url is not configured (set GENTLY_COLLECTOR_URL or config.toml)"
        );
        validate_collector_url(&self.collector_url)
    }

    /// Validate that a collector URL + token are configured (for export/query).
    pub fn require_collector(&self) -> Result<()> {
        self.require_collector_url()?;
        anyhow::ensure!(
            !self.token.is_empty(),
            "token is not configured (supply GENTLY_TOKEN through your secret provider)"
        );
        self.validate()
    }
}

fn validate_collector_url(collector_url: &str) -> Result<()> {
    let url =
        reqwest::Url::parse(collector_url).map_err(|_| anyhow::anyhow!("invalid collector URL"))?;
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

/// An explicitly supplied duration must be usable rather than silently ignored.
fn env_u64(key: &str) -> Result<Option<u64>> {
    match std::env::var(key) {
        Ok(value) => {
            value.trim().parse().map(Some).map_err(|_| {
                anyhow::anyhow!("{key} must be an unsigned integer between 1 and 3600")
            })
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => anyhow::bail!("{key} must be an unsigned integer between 1 and 3600"),
    }
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
