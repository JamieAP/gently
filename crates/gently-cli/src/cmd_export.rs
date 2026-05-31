//! `gently export` - drain the local outbox to the collector.
//!
//! A single exporter runs at a time, enforced with an advisory file lock that
//! the OS releases automatically on process death (so a crashed exporter leaves
//! no stale lock - convergence is preserved). If another exporter holds the
//! lock we exit immediately; it is already draining the shared outbox.

use crate::config::Config;
use anyhow::{Context, Result};
use fs4::fs_std::FileExt;
use gently_export::{drain, Http2Transport, PreferQuic, QuicTransport};
use gently_store::Store;

pub fn run() -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    init_log(&cfg);

    let lock_path = cfg.state_dir.join("export.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening {}", lock_path.display()))?;

    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            tracing::debug!("another exporter holds the lock; exiting");
            return Ok(());
        }
        Err(e) => return Err(e).context("acquiring export lock"),
    }

    cfg.require_collector()?;
    let store = Store::open(&cfg.state_db())?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    // The QUIC (HTTP/3) client must be built inside the tokio runtime - reqwest
    // spawns the quinn endpoint driver on the current runtime, so constructing
    // it outside fails with "no async runtime found". Build transports and drain
    // within one block_on; prefer QUIC, fall back to HTTP/2 (logged).
    let result = runtime.block_on(async {
        let http2 = Http2Transport::new(&cfg.collector_url, &cfg.token, cfg.export_timeout_secs);
        // Only build the QUIC client when preferred (config `prefer_quic`).
        let quic = if cfg.prefer_quic {
            match QuicTransport::new(&cfg.collector_url, &cfg.token, cfg.export_timeout_secs) {
                Ok(q) => {
                    tracing::info!("QUIC (HTTP/3) transport built; preferring it over HTTP/2");
                    Some(q)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "QUIC client unavailable; using HTTP/2 only");
                    None
                }
            }
        } else {
            tracing::info!("prefer_quic=false; using HTTP/2");
            None
        };
        let transport = PreferQuic::new(quic, http2);
        drain(&store, &transport, cfg.outbox_cap, cfg.export_batch).await
    });
    // Release before reporting; the lock also drops at end of scope.
    let _ = FileExt::unlock(&lock);

    match result {
        Ok(n) => {
            tracing::info!(delivered = n, "export drain complete");
            Ok(())
        }
        // A failed batch is expected when the collector is down; the outbox kept
        // the rows. Surface it for the CLI but it is not a crash condition.
        Err(e) => Err(e).context("export drain"),
    }
}

/// Log to `<state_dir>/export.log` so the detached exporter's transport choices
/// (QUIC vs HTTP/2 fallback) and drain results are observable after the fact.
fn init_log(cfg: &Config) {
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(cfg.state_dir.join("export.log"))
    {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .try_init();
    }
}
