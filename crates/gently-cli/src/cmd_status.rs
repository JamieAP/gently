//! `gently status` - local exporter health and queue depth.
//!
//! Reads the store's health row + queue/quarantine depth so a silently-failing
//! detached exporter is observable without any daemon: the *query* tells you
//! whether spans are flowing.

use crate::config::Config;
use anyhow::Result;
use comfy_table::{Cell, Table};
use gently_store::Store;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn run() -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    let store = Store::open(&cfg.state_db())?;

    let pending = store.outbox_len()?;
    let quarantined = store.quarantine_len()?;
    let h = store.health_snapshot()?;
    let raw = store.raw_objects_stats(&cfg.tenant_id)?;
    // Status remains useful for invalid configuration, but must not echo URL
    // userinfo, paths or query strings that might contain private values.
    let collector = if cfg.collector_url.is_empty() {
        "-".to_string()
    } else {
        reqwest::Url::parse(&cfg.collector_url)
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_else(|_| "invalid URL".into())
    };

    let mut t = Table::new();
    t.set_header(vec!["field", "value"]);
    t.add_row(vec![Cell::new("collector_url"), Cell::new(collector)]);
    t.add_row(vec![Cell::new("prefer_quic"), Cell::new(cfg.prefer_quic)]);
    t.add_row(vec![Cell::new("pending (outbox)"), Cell::new(pending)]);
    t.add_row(vec![Cell::new("quarantined"), Cell::new(quarantined)]);
    t.add_row(vec![Cell::new("tenant_id"), Cell::new(&cfg.tenant_id)]);
    t.add_row(vec![Cell::new("device_id"), Cell::new(&cfg.device_id)]);
    t.add_row(vec![Cell::new("raw_objects"), Cell::new(raw.objects)]);
    t.add_row(vec![Cell::new("raw_bytes"), Cell::new(raw.bytes)]);
    t.add_row(vec![
        Cell::new("raw_pending"),
        Cell::new(raw.pending_objects),
    ]);
    t.add_row(vec![
        Cell::new("raw_pending_bytes"),
        Cell::new(raw.pending_bytes),
    ]);
    t.add_row(vec![
        Cell::new("raw_budget_bytes"),
        Cell::new(gently_store::RAW_OBJECT_CAP_BYTES),
    ]);
    t.add_row(vec![
        Cell::new("consecutive_failures"),
        Cell::new(h.consecutive_failures),
    ]);
    t.add_row(vec![
        Cell::new("last_export"),
        Cell::new(ago(h.last_attempt_unix_nano)),
    ]);
    t.add_row(vec![
        Cell::new("last_success"),
        Cell::new(ago(h.last_ok_unix_nano)),
    ]);
    t.add_row(vec![
        Cell::new("last_error"),
        Cell::new(h.last_error.as_deref().unwrap_or("-")),
    ]);
    println!("{t}");
    Ok(())
}

/// Human-readable "time since" for a unix-nanos instant.
fn ago(when: Option<u64>) -> String {
    let Some(when) = when else {
        return "never".into();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    if now < when {
        return "just now".into();
    }
    let secs = (now - when) / 1_000_000_000;
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        _ => format!("{}h ago", secs / 3600),
    }
}
