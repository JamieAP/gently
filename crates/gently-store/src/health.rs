//! Exporter health and outbox quarantine.
//!
//! The exporter records its latest drain outcome in a health row. Its error
//! policy selects rejected or invalid queued envelopes for quarantine and names
//! a typed [`QuarantineReason`]; this storage helper does not classify HTTP
//! responses. Each quarantine row retains
//! the whole queued envelope, which may contain multiple spans. `gently status`
//! reads health and quarantine counts without requiring a daemon.

use crate::{QuarantineReason, Result, Store};

/// A snapshot of the exporter's last-known health.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Health {
    pub last_attempt_unix_nano: Option<u64>,
    pub last_ok_unix_nano: Option<u64>,
    pub last_error: Option<String>,
    pub consecutive_failures: u64,
}

impl Store {
    /// Move whole queued envelopes into quarantine with a typed reason.
    /// Returns how many outbox rows were quarantined.
    pub fn outbox_quarantine(&self, ids: &[i64], reason: QuarantineReason) -> Result<usize> {
        let reason = reason.encode();
        let tx = self.conn.unchecked_transaction()?;
        let mut moved = 0;
        for id in ids {
            moved += tx.execute(
                "INSERT INTO quarantine (span_json, reason, quarantined_unix_nano)
                 SELECT span_json, ?2, strftime('%s','now') * 1000000000
                 FROM outbox WHERE id = ?1",
                rusqlite::params![id, &reason],
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
    use crate::{QuarantineReason, Store};

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
        let moved = s
            .outbox_quarantine(
                &ids[..1],
                QuarantineReason::CollectorRejection { status: 400 },
            )
            .unwrap();
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
