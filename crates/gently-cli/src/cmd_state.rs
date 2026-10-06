//! Keyless, namespace-pinned encrypted-state recovery.
use crate::config::Config;
use anyhow::{ensure, Result};
use clap::Subcommand;
use gently_store::{recovery_leftovers, remove_recovery_leftovers, Leftover, Store};
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum StateCommand {
    /// Make a consistent private SQLite backup (including live WAL changes).
    Backup {
        path: PathBuf,
        /// Delete private temporaries left in the destination directory by an
        /// interrupted backup. Only exact owner-only Gently temporaries qualify.
        #[arg(long)]
        remove_stale: bool,
    },
    /// Restore into missing state.db; stop Gently first. Never replaces state.
    Restore { path: PathBuf },
}
pub fn run(command: StateCommand) -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    match command {
        StateCommand::Backup { path, remove_stale } => {
            ensure!(
                cfg.state_db().exists(),
                "no local state database to back up"
            );
            for line in leftover_report(&path, remove_stale)? {
                eprintln!("{line}");
            }
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

/// Surface partial private copies from interrupted backups before making a
/// new one. Removal is opt-in because a concurrent backup may still own them.
fn leftover_report(destination: &Path, remove: bool) -> Result<Vec<String>> {
    let (ours, unverified): (Vec<_>, Vec<_>) = recovery_leftovers(destination)?
        .into_iter()
        .partition(|leftover| matches!(leftover, Leftover::Ours(_)));
    let mut lines = Vec::new();
    match (ours.len(), remove) {
        (0, _) => (),
        (_, true) => lines.push(format!(
            "gently: removed {} interrupted backup temporary file(s) from the destination directory.",
            remove_recovery_leftovers(&ours)?
        )),
        (found, false) => lines.push(format!(
            "gently: found {found} interrupted backup temporary file(s) (.gently-recovery-*.db*) holding private metadata in the destination directory; once no other backup or restore is running, rerun with --remove-stale to delete them."
        )),
    }
    if !unverified.is_empty() {
        lines.push(format!(
            "gently: left {} file(s) named like backup temporaries that are linked, not regular, not owner-only or not exactly Gently's; review them manually.",
            unverified.len()
        ));
    }
    Ok(lines)
}
