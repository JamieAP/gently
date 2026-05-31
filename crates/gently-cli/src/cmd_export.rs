//! `gently export` - drain the local outbox to the collector.
//!
//! A single exporter runs at a time, enforced with an advisory file lock that
//! the OS releases automatically on process death (so a crashed exporter leaves
//! no stale lock - convergence is preserved). If another exporter holds the
//! lock we exit immediately; it is already draining the shared outbox.

use crate::config::Config;
use anyhow::{Context, Result};
use fs4::fs_std::FileExt;
use gently_export::{drain, Http2Transport};
use gently_store::Store;

pub fn run() -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;

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
    let transport = Http2Transport::new(&cfg.collector_url, &cfg.token);
    let store = Store::open(&cfg.state_db())?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    let result = runtime.block_on(drain(&store, &transport));
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
