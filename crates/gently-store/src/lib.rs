//! Local durable state for the hook pipeline: a single SQLite database
//! (`~/.gently/state.db`) holding two things -
//!
//! - `open_spans`: spans awaiting their closing hook event (a turn awaiting
//!   `Stop`, a tool call awaiting `PostToolUse`), and
//! - `outbox`: completed OTLP span JSON awaiting export to the collector.
//!
//! WAL mode + a busy timeout let the many short-lived hook processes (including
//! parallel tool calls and subagents) write concurrently without "database is
//! locked" errors. A down collector simply means the outbox grows and the next
//! exporter run retries - the durability that makes the pipeline self-healing.

mod health;
mod open_spans;
mod outbox;

pub use health::Health;
pub use open_spans::OpenSpan;

use std::path::Path;

/// Hard cap on buffered outbox rows; older rows are dropped beyond this so a
/// long collector outage cannot grow the db without bound.
pub const OUTBOX_CAP: usize = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Handle to the local state database. Cheap to open per process.
pub struct Store {
    conn: rusqlite::Connection,
}

impl Store {
    /// Open (creating if needed) the state db at `path`, enabling WAL and the
    /// busy timeout, and ensuring the schema exists.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = rusqlite::Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS open_spans (
  session_id TEXT NOT NULL,
  logical_key TEXT NOT NULL,
  span_id TEXT NOT NULL,
  parent_span_id TEXT,
  name TEXT NOT NULL,
  kind INTEGER NOT NULL,
  start_unix_nano INTEGER NOT NULL,
  attrs_json TEXT NOT NULL,
  PRIMARY KEY (session_id, logical_key)
);
CREATE TABLE IF NOT EXISTS outbox (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  span_json TEXT NOT NULL,
  created_unix_nano INTEGER NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS counters (
  session_id TEXT PRIMARY KEY,
  turn_index INTEGER NOT NULL DEFAULT 0,
  current_turn INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS quarantine (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  span_json TEXT NOT NULL,
  reason TEXT NOT NULL,
  quarantined_unix_nano INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS health (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  last_attempt_unix_nano INTEGER,
  last_ok_unix_nano INTEGER,
  last_error TEXT,
  consecutive_failures INTEGER NOT NULL DEFAULT 0
);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_writers_do_not_lock_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        Store::open(&path).unwrap();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let p = path.clone();
                std::thread::spawn(move || {
                    let s = Store::open(&p).unwrap();
                    for j in 0..50 {
                        s.outbox_enqueue(&format!("{{\"n\":\"{i}_{j}\"}}")).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.outbox_len().unwrap(), 400);
    }
}
