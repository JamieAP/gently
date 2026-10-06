//! Local durable state for the hook pipeline: a single SQLite database
//! (`~/.gently/state.db`) holding lifecycle state, telemetry envelopes and
//! opaque encrypted raw objects. Raw plaintext is never a database value.
//!
//! - `open_spans`: spans awaiting their closing hook event (a turn awaiting
//!   `Stop`, a tool call awaiting `PostToolUse`), and
//! - `outbox`: completed OTLP span JSON awaiting export to the collector.
//!
//! WAL mode + a busy timeout let the many short-lived hook processes (including
//! parallel tool calls and subagents) write concurrently without "database is
//! locked" errors. A down collector simply means the outbox grows and the next
//! exporter run retries - the durability that makes the pipeline self-healing.

mod backup;
mod health;
mod open_spans;
mod outbox;
pub mod private_fs;
mod raw_objects;

pub use backup::{recovery_leftovers, remove_recovery_leftovers, Leftover};
pub use health::{CaptureHealth, CaptureOutcome, Health};
pub use open_spans::OpenSpan;
pub use raw_objects::RawObjectStats;

use std::path::Path;

/// Hard cap on buffered outbox rows; older rows are dropped beyond this so a
/// long collector outage cannot grow the db without bound.
pub const OUTBOX_CAP: usize = 10_000;
/// Encrypted object JSON budget per tenant/device database. Never evicts rows.
pub const RAW_OBJECT_CAP_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("private state file: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("incompatible development state schema; stop Gently and explicitly reset its state database before continuing")]
    IncompatibleSchema,
    #[error("invalid or incompatible encrypted-state backup")]
    InvalidBackup,
    #[error("backup tenant/device differs from the configured local namespace")]
    BackupNamespace,
    #[error("backup could not acquire a consistent snapshot; pause writers and retry")]
    BackupBusy,
    #[error("invalid encrypted raw object")]
    InvalidRawObject,
    #[error("encrypted raw reference already names a different object")]
    RawObjectConflict,
    #[error("encrypted raw store byte budget reached; existing ciphertext is retained")]
    RawCapacity,
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
        // Precreate and harden the database before SQLite creates WAL sidecars.
        // Do not change the caller's parent directory (it may be a shared /tmp).
        private_fs::prepare_sqlite_file(path)?;
        let sidecars: Vec<std::path::PathBuf> = ["-wal", "-shm", "-journal"]
            .iter()
            .map(|suffix| {
                let mut name = path.as_os_str().to_os_string();
                name.push(suffix);
                std::path::PathBuf::from(name)
            })
            .collect();
        for sidecar in &sidecars {
            private_fs::harden_existing_file(sidecar)?;
        }
        let conn = rusqlite::Connection::open(path)?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let tables: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )?;
        if version != SCHEMA_VERSION && (version != 0 || tables != 0) {
            return Err(StoreError::IncompatibleSchema);
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        for sidecar in &sidecars {
            private_fs::harden_existing_file(sidecar)?;
        }
        Ok(Self { conn })
    }

    /// Commit an event's lifecycle changes, encrypted raw object and telemetry
    /// envelope together. Any failure rolls every write back.
    pub fn transaction<T, E>(
        &self,
        f: impl FnOnce(&Self) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<StoreError>,
    {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(StoreError::from)?;
        let result = f(self)?;
        tx.commit().map_err(StoreError::from)?;
        Ok(result)
    }

    /// Pending span identities and attributes, used to authenticate raw fields
    /// captured at an opening event to the span later completed by a close.
    pub fn open_span_attributes(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT span_id, attrs_json FROM open_spans")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Limit encryption binding work to the opening spans bearing this random
    /// ref, instead of deserializing every live session on each hook.
    pub fn open_span_attributes_with_reference(
        &self,
        raw_ref: &str,
    ) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT span_id, attrs_json FROM open_spans WHERE instr(attrs_json, ?1) > 0",
        )?;
        let rows = stmt
            .query_map([raw_ref], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

const SCHEMA_VERSION: i64 = 2;

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
CREATE TABLE IF NOT EXISTS turn_ordinals (
  session_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  PRIMARY KEY (session_id, turn_id)
);
CREATE TABLE IF NOT EXISTS quarantine (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  span_json TEXT NOT NULL,
  reason TEXT NOT NULL,
  quarantined_unix_nano INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS raw_objects (
  tenant_id TEXT NOT NULL,
  raw_ref TEXT NOT NULL,
  object_json TEXT NOT NULL,
  created_unix_nano INTEGER NOT NULL,
  synced INTEGER NOT NULL DEFAULT 0,
  rejected_status INTEGER,
  rejected_unix_nano INTEGER,
  PRIMARY KEY (tenant_id, raw_ref)
);
CREATE INDEX IF NOT EXISTS raw_objects_pending ON raw_objects (tenant_id, synced, created_unix_nano);
CREATE TABLE IF NOT EXISTS capture_health (
  outcome TEXT PRIMARY KEY,
  count INTEGER NOT NULL,
  last_unix_nano INTEGER NOT NULL
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
    fn capture_panic_rolls_back_all_tables_and_connection_can_commit_again() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        let span = OpenSpan {
            session_id: "s".into(),
            logical_key: "tool:u".into(),
            span_id: "abcd".into(),
            parent_span_id: None,
            name: "Bash".into(),
            kind: 3,
            start_unix_nano: 42,
            attrs_json: "[]".into(),
        };
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<()> = store.transaction(|s| {
                s.next_turn_index("s")?;
                s.turn_ordinal("s", "t")?;
                s.open_span(&span)?;
                s.outbox_enqueue("{}")?;
                panic!("fixture capture panic");
            });
        }));
        assert!(panicked.is_err());
        assert_eq!(store.outbox_len().unwrap(), 0);
        assert_eq!(store.current_turn("s").unwrap(), 0);
        assert!(store.peek_open("s", "tool:u").unwrap().is_none());
        store
            .transaction(|s| -> Result<()> {
                assert_eq!(s.next_turn_index("s")?, 1);
                assert_eq!(s.turn_ordinal("s", "t")?, (1, true));
                s.open_span(&span)?;
                s.outbox_enqueue("{}")
            })
            .unwrap();
        assert_eq!(store.outbox_len().unwrap(), 1);
        assert_eq!(store.peek_open("s", "tool:u").unwrap(), Some(span));
    }

    #[test]
    fn concurrent_transactions_keep_each_counter_with_its_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        Store::open(&path).unwrap();
        let writers: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let store = Store::open(&path).unwrap();
                    for _ in 0..50 {
                        store
                            .transaction(|s| -> Result<()> {
                                let turn = s.next_turn_index("shared")?;
                                std::thread::yield_now();
                                assert_eq!(s.current_turn("shared")?, turn);
                                s.outbox_enqueue(&turn.to_string())
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let store = Store::open(&path).unwrap();
        let turns: Vec<u64> = store
            .outbox_take_batch(500)
            .unwrap()
            .iter()
            .map(|(_, json)| json.parse().unwrap())
            .collect();
        assert_eq!(turns, (1..=400).collect::<Vec<_>>());
    }

    #[test]
    fn legacy_plaintext_schema_requires_an_explicit_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE raw_values (sha256 TEXT PRIMARY KEY, value TEXT, created_unix_nano INTEGER);").unwrap();
        drop(conn);
        assert!(
            Store::open(&path).is_err(),
            "a plaintext development schema must not be silently reused"
        );
    }

    #[test]
    fn failed_event_transaction_rolls_back_lifecycle_and_outbox() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        let result: Result<()> = store.transaction(|store| {
            store.next_turn_index("fixture")?;
            store.outbox_enqueue("synthetic envelope")?;
            Err(StoreError::InvalidRawObject)
        });
        assert!(result.is_err());
        assert_eq!(store.current_turn("fixture").unwrap(), 0);
        assert_eq!(store.outbox_len().unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn database_path_does_not_follow_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("unrelated.db");
        let _existing = Store::open(&target).unwrap();
        let link = dir.path().join("state.db");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(Store::open(&link).is_err(), "state path followed a symlink");
    }

    #[cfg(unix)]
    #[test]
    fn database_and_wal_sidecars_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let store = Store::open(&path).unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let file = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
            if file.exists() {
                std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
        }
        let reopened = Store::open(&path).unwrap();
        reopened.outbox_enqueue("synthetic envelope").unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let file = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(store);
    }

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
