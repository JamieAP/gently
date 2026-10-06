//! Payload-free metadata quarantine inspection and explicit local retry.
use crate::config::Config;
use anyhow::{ensure, Result};
use clap::Subcommand;
use gently_store::Store;

#[derive(Subcommand)]
pub enum QuarantineCommand {
    /// List summaries as JSON; never print retained envelope content.
    List {
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=1000))]
        limit: u32,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(i64).range(0..))]
        after_id: i64,
    },
    /// Requeue one retained envelope; fix its producing/collector issue first.
    Retry {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        id: i64,
    },
}
pub fn run(command: QuarantineCommand) -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    let store = Store::open(&cfg.state_db())?;
    match command {
        QuarantineCommand::List { limit, after_id } => {
            let rows = store.quarantine_summaries(after_id, limit as usize)?;
            println!("{}", serde_json::json!(rows.iter().map(|row| serde_json::json!({
                "id": row.id, "bytes": row.bytes, "quarantined_unix_nano": row.quarantined_unix_nano,
                "category": row.category
            })).collect::<Vec<_>>()));
        }
        QuarantineCommand::Retry { id } => {
            ensure!(
                store.quarantine_retry(id)?,
                "quarantine row not found in this local namespace"
            );
            println!(
                "Retained envelope requeued; run gently export to retry authenticated delivery."
            );
        }
    }
    Ok(())
}
