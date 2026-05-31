//! `gently export` - drain the local outbox to the collector.
//!
//! A single exporter runs at a time, enforced with an advisory file lock that
//! the OS releases automatically on process death (so a crashed exporter leaves
//! no stale lock - convergence is preserved). If another exporter holds the
//! lock we exit immediately; it is already draining the shared outbox.
//!
//! Within a run we retry a retryable failure (collector down / 5xx / timeout) a
//! few times with exponential backoff before giving up - the next hook-spawned
//! run retries beyond that. A 4xx-rejected (poison) span is quarantined inside
//! `drain`, never retried. The run's outcome is recorded in the store's health
//! row so `gently status` can surface a silently-failing exporter.

use crate::config::Config;
use anyhow::{Context, Result};
use fs4::fs_std::FileExt;
use gently_export::{drain, Http2Transport, PreferQuic, QuicTransport};
use gently_store::Store;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// In-run retry budget for retryable failures, and the base backoff.
const MAX_RETRIES: u32 = 3;
const BACKOFF_BASE: Duration = Duration::from_millis(250);

pub fn run() -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    crate::logging::init_file_log(&cfg.state_dir.join("export.log"));

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
    // it outside fails with "no async runtime found". Build transports once and
    // run the retry/backoff loop inside one block_on.
    let result = runtime.block_on(async {
        let http2 = Http2Transport::new(&cfg.collector_url, &cfg.token, cfg.export_timeout_secs);
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

        // Retry retryable failures with exponential backoff; a non-retryable
        // error never escapes drain (poison is quarantined inside it).
        let mut last_err = None;
        for attempt in 0..MAX_RETRIES {
            match drain(&store, &transport, cfg.outbox_cap, cfg.export_batch).await {
                Ok(n) => return Ok(n),
                Err(e) => {
                    let backoff = BACKOFF_BASE * 2u32.pow(attempt);
                    tracing::warn!(error = %e, attempt = attempt + 1, ?backoff, "export retry");
                    last_err = Some(e);
                    tokio::time::sleep(backoff).await;
                }
            }
        }
        Err(last_err.expect("loop ran at least once"))
    });
    // Release before reporting; the lock also drops at end of scope.
    let _ = FileExt::unlock(&lock);

    // Record health so a silently-failing detached exporter is visible to
    // `gently status` (best-effort: never fail the run on a health write).
    let now = now_nanos();
    match &result {
        Ok(n) => {
            tracing::info!(delivered = n, "export drain complete");
            let _ = store.health_record_success(now);
            Ok(())
        }
        // Expected when the collector is down; the outbox kept the rows.
        Err(e) => {
            let _ = store.health_record_failure(now, &e.to_string());
            Err(anyhow::anyhow!("{e}")).context("export drain")
        }
    }
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
