//! Payload-free metadata quarantine inspection and explicit local retry.
use crate::config::Config;
use anyhow::{ensure, Result};
use clap::Subcommand;
use gently_store::{QuarantineSummary, Store};

#[derive(Subcommand)]
pub enum QuarantineCommand {
    /// List summaries as JSON; never print retained envelope content.
    List {
        /// Maximum summaries to print (1-1000).
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=1000))]
        limit: u32,
        /// Print rows with an ID greater than this; pass the last printed ID
        /// to continue.
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(i64).range(0..))]
        after_id: i64,
    },
    /// Requeue one retained envelope; fix its producing/collector issue first.
    Retry {
        /// Quarantine row ID from `gently quarantine list`.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        id: i64,
    },
}

pub fn run(command: QuarantineCommand) -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    let store = Store::open(&cfg.state_db())?;
    match command {
        QuarantineCommand::List { limit, after_id } => list(&store, after_id, limit),
        QuarantineCommand::Retry { id } => retry(&store, id),
    }
}

fn list(store: &Store, after_id: i64, limit: u32) -> Result<()> {
    let rows = store.quarantine_summaries(after_id, limit as usize)?;
    let rows: Vec<_> = rows.iter().map(summary_json).collect();
    println!("{}", serde_json::Value::Array(rows));
    Ok(())
}

fn retry(store: &Store, id: i64) -> Result<()> {
    ensure!(
        store.quarantine_retry(id)?,
        "quarantine row not found in this local namespace"
    );
    println!("Retained envelope requeued; run gently export to retry authenticated delivery.");
    Ok(())
}

/// Only typed metadata leaves the store, so no payload or free-form reason
/// text can reach stdout.
fn summary_json(row: &QuarantineSummary) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "bytes": row.bytes,
        "quarantined_unix_nano": row.quarantined_unix_nano,
        "category": row.reason.map_or("other", |reason| reason.code()),
        "http_status": row.reason.and_then(|reason| reason.http_status()),
    })
}
