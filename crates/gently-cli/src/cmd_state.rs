//! Keyless, namespace-pinned encrypted-state recovery.
use crate::config::Config;
use anyhow::{ensure, Result};
use clap::Subcommand;
use gently_store::Store;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum StateCommand {
    /// Make a consistent private SQLite backup (including live WAL changes).
    Backup { path: PathBuf },
    /// Restore into missing state.db; stop Gently first. Never replaces state.
    Restore { path: PathBuf },
}
pub fn run(command: StateCommand) -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    match command {
        StateCommand::Backup { path } => {
            ensure!(
                cfg.state_db().exists(),
                "no local state database to back up"
            );
            Store::open(&cfg.state_db())?.backup(&path, &cfg.tenant_id, &cfg.device_id)?;
            println!("Created a private consistent state backup; store policy and reader recovery separately.");
        }
        StateCommand::Restore { path } => {
            Store::restore_backup(&path, &cfg.state_db(), &cfg.tenant_id, &cfg.device_id)?;
            println!("Restored encrypted state for the configured namespace; verify policy and gently status before restarting.");
        }
    }
    Ok(())
}
