//! Shared file logging for the hook and exporter processes.
//!
//! Both are short-lived, separately-spawned processes that append to a log in
//! the state dir. Each run rotates the file once it crosses a size cap (keeping
//! one `.1` backup) so an append-only log can't grow without bound - no daemon,
//! no logrotate, just a check at process start.

use std::path::Path;

const LOG_CAP_BYTES: u64 = 5 * 1024 * 1024;

/// Initialise a file-backed tracing subscriber at `path`, rotating first if the
/// existing file exceeds the cap. Best-effort: logging never fails the caller.
pub fn init_file_log(path: &Path) {
    // Harden both current and retained logs before rotation or append.
    if gently_store::private_fs::harden_existing_file(path).is_err() { return; }
    if let Some(name) = path.file_name() {
        let rotated = path.with_file_name(format!("{}.1", name.to_string_lossy()));
        if gently_store::private_fs::harden_existing_file(&rotated).is_err() { return; }
    }
    rotate_if_large(path, LOG_CAP_BYTES);
    if let Ok(file) = gently_store::private_fs::open_private_file(path, true)
    {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .try_init();
    }
}

/// Rename `foo.log` → `foo.log.1` (overwriting any prior backup) when it grows
/// past `cap`, so the next run starts a fresh file.
fn rotate_if_large(path: &Path, cap: u64) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= cap {
        return;
    }
    if let Some(name) = path.file_name() {
        let rotated = path.with_file_name(format!("{}.1", name.to_string_lossy()));
        let _ = std::fs::rename(path, rotated);
    }
}
