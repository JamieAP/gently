//! Exporter health and outbox quarantine.
//!
//! The exporter records its latest drain outcome in a health row. Its error
//! policy selects rejected or invalid queued envelopes for quarantine; this
//! storage helper does not classify HTTP responses. Each quarantine row retains
//! the whole queued envelope, which may contain multiple spans. `gently status`
//! reads health and quarantine counts without requiring a daemon.

use crate::{Result, Store};

/// A snapshot of the exporter's last-known health.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Health {
    pub last_attempt_unix_nano: Option<u64>,
    pub last_ok_unix_nano: Option<u64>,
    pub last_error: Option<String>,
    pub consecutive_failures: u64,
}

/// Fixed categories only: capture health never stores payloads or error strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureOutcome {
    MetadataOnly,
    Encrypted,
    PolicyUnavailable,
    PolicyExpired,
    Oversized,
    RawCapacity,
    SealFailed,
    InvalidHook,
    CaptureFailed,
}
impl CaptureOutcome {
    pub const ALL: [Self; 9] = [
        Self::MetadataOnly,
        Self::Encrypted,
        Self::PolicyUnavailable,
        Self::PolicyExpired,
        Self::Oversized,
        Self::RawCapacity,
        Self::SealFailed,
        Self::InvalidHook,
        Self::CaptureFailed,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::MetadataOnly => "metadata_only",
            Self::Encrypted => "encrypted",
            Self::PolicyUnavailable => "policy_unavailable",
            Self::PolicyExpired => "policy_expired",
            Self::Oversized => "oversized",
            Self::RawCapacity => "raw_capacity",
            Self::SealFailed => "seal_failed",
            Self::InvalidHook => "invalid_hook",
            Self::CaptureFailed => "capture_failed",
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct CaptureHealth {
    pub last_capture_unix_nano: Option<u64>,
    pub last_outcome: Option<CaptureOutcome>,
    pub counts: std::collections::BTreeMap<&'static str, u64>,
}

impl Store {
    pub fn capture_record(&self, outcome: CaptureOutcome, now: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO capture_health (outcome, count, last_unix_nano)
            VALUES (?1, 1, ?2) ON CONFLICT(outcome) DO UPDATE SET
            count = count + 1, last_unix_nano = max(last_unix_nano, ?2)",
            rusqlite::params![outcome.label(), now as i64],
        )?;
        Ok(())
    }
    pub fn capture_snapshot(&self) -> Result<CaptureHealth> {
        let mut snapshot = CaptureHealth::default();
        for outcome in CaptureOutcome::ALL {
            snapshot.counts.insert(outcome.label(), 0);
        }
        // One SELECT gives a consistent SQLite snapshot under concurrent hooks.
        let mut query = self
            .conn
            .prepare("SELECT outcome, count, last_unix_nano FROM capture_health")?;
        let rows = query.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, i64>(2)? as u64,
            ))
        })?;
        for row in rows {
            let (label, count, when) = row?;
            let Some(outcome) = CaptureOutcome::ALL.into_iter().find(|v| v.label() == label) else {
                continue;
            };
            snapshot.counts.insert(outcome.label(), count);
            if count > 0
                && snapshot
                    .last_capture_unix_nano
                    .is_none_or(|last| when > last)
            {
                snapshot.last_capture_unix_nano = Some(when);
                snapshot.last_outcome = Some(outcome);
            }
        }
        Ok(snapshot)
    }

    /// Move whole queued envelopes into quarantine with a reason.
    /// Returns how many outbox rows were quarantined.
    pub fn outbox_quarantine(&self, ids: &[i64], reason: &str) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut moved = 0;
        for id in ids {
            moved += tx.execute(
                "INSERT INTO quarantine (span_json, reason, quarantined_unix_nano)
                 SELECT span_json, ?2, strftime('%s','now') * 1000000000
                 FROM outbox WHERE id = ?1",
                rusqlite::params![id, reason],
            )?;
            tx.execute("DELETE FROM outbox WHERE id = ?1", rusqlite::params![id])?;
        }
        tx.commit()?;
        Ok(moved)
    }

    /// Number of quarantined envelopes, rather than the spans they contain.
    pub fn quarantine_len(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM quarantine", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// Record a successful drain: clears the failure streak and last error.
    pub fn health_record_success(&self, now_nanos: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO health (id, last_attempt_unix_nano, last_ok_unix_nano, consecutive_failures, last_error)
             VALUES (1, ?1, ?1, 0, NULL)
             ON CONFLICT(id) DO UPDATE SET
               last_attempt_unix_nano = ?1, last_ok_unix_nano = ?1,
               consecutive_failures = 0, last_error = NULL",
            rusqlite::params![now_nanos as i64],
        )?;
        Ok(())
    }

    /// Record a failed drain: bumps the failure streak and stores the error.
    pub fn health_record_failure(&self, now_nanos: u64, error: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO health (id, last_attempt_unix_nano, consecutive_failures, last_error)
             VALUES (1, ?1, 1, ?2)
             ON CONFLICT(id) DO UPDATE SET
               last_attempt_unix_nano = ?1,
               consecutive_failures = consecutive_failures + 1,
               last_error = ?2",
            rusqlite::params![now_nanos as i64, error],
        )?;
        Ok(())
    }

    /// The current health snapshot (defaults when nothing recorded yet).
    pub fn health_snapshot(&self) -> Result<Health> {
        let snap = self
            .conn
            .query_row(
                "SELECT last_attempt_unix_nano, last_ok_unix_nano, last_error, consecutive_failures
                 FROM health WHERE id = 1",
                [],
                |r| {
                    Ok(Health {
                        last_attempt_unix_nano: r.get::<_, Option<i64>>(0)?.map(|v| v as u64),
                        last_ok_unix_nano: r.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                        last_error: r.get(2)?,
                        consecutive_failures: r.get::<_, i64>(3)? as u64,
                    })
                },
            )
            .unwrap_or_default();
        Ok(snap)
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("state.db")).unwrap();
        (dir, s)
    }

    #[test]
    fn quarantine_moves_rows_out_of_outbox() {
        let (_d, s) = store();
        s.outbox_enqueue("{\"a\":1}").unwrap();
        s.outbox_enqueue("{\"a\":2}").unwrap();
        let ids: Vec<i64> = s
            .outbox_take_batch(10)
            .unwrap()
            .iter()
            .map(|(id, _)| *id)
            .collect();
        let moved = s.outbox_quarantine(&ids[..1], "rejected 400").unwrap();
        assert_eq!(moved, 1);
        assert_eq!(s.outbox_len().unwrap(), 1);
        assert_eq!(s.quarantine_len().unwrap(), 1);
    }

    #[test]
    fn health_tracks_failure_streak_then_clears() {
        let (_d, s) = store();
        assert_eq!(s.health_snapshot().unwrap().consecutive_failures, 0);
        s.health_record_failure(100, "boom").unwrap();
        s.health_record_failure(200, "boom again").unwrap();
        let h = s.health_snapshot().unwrap();
        assert_eq!(h.consecutive_failures, 2);
        assert_eq!(h.last_error.as_deref(), Some("boom again"));
        assert_eq!(h.last_attempt_unix_nano, Some(200));
        s.health_record_success(300).unwrap();
        let h = s.health_snapshot().unwrap();
        assert_eq!(h.consecutive_failures, 0);
        assert_eq!(h.last_error, None);
        assert_eq!(h.last_ok_unix_nano, Some(300));
    }
}

#[cfg(test)]
mod capture_tests {
    use crate::{CaptureOutcome, Store};
    #[test]
    fn capture_extension_reopens_existing_encrypted_schema_without_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let store = Store::open(&path).unwrap();
            store.outbox_enqueue("synthetic metadata").unwrap();
        }
        {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute("DROP TABLE capture_health", []).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.outbox_len().unwrap(), 1);
        assert!(store.capture_snapshot().unwrap().last_outcome.is_none());
        for (when, outcome) in [
            (1, CaptureOutcome::Encrypted),
            (2, CaptureOutcome::RawCapacity),
            (3, CaptureOutcome::Encrypted),
        ] {
            store.capture_record(outcome, when).unwrap();
        }
        let health = store.capture_snapshot().unwrap();
        assert_eq!(health.last_outcome, Some(CaptureOutcome::Encrypted));
        assert_eq!(health.counts["raw_capacity"], 1);
        assert_eq!(health.counts["encrypted"], 2);
    }
}
