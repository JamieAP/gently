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

/// Bounded operational summary. Never exposes queued JSON or stored reason text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuarantineSummary {
    pub id: i64,
    pub bytes: u64,
    pub quarantined_unix_nano: u64,
    pub category: &'static str,
}

impl Store {
    pub fn quarantine_summaries(
        &self,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<QuarantineSummary>> {
        let mut query = self.conn.prepare(
            "SELECT id, length(CAST(span_json AS BLOB)), quarantined_unix_nano,
            CASE WHEN reason = 'invalid queued OTLP envelope JSON' THEN 'invalid_json'
                 WHEN reason LIKE 'collector rejected%' THEN 'collector_rejection' ELSE 'other' END
            FROM quarantine WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = query
            .query_map(
                rusqlite::params![after_id.max(0), limit.clamp(1, 1000) as i64],
                |r| {
                    let category: String = r.get(3)?;
                    Ok(QuarantineSummary {
                        id: r.get(0)?,
                        bytes: r.get::<_, i64>(1)? as u64,
                        quarantined_unix_nano: r.get::<_, i64>(2)? as u64,
                        category: match category.as_str() {
                            "invalid_json" => "invalid_json",
                            "collector_rejection" => "collector_rejection",
                            _ => "other",
                        },
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Atomically move one retained envelope to the active namespace's queue.
    /// Export authenticates independently before sending; retry never decrypts.
    pub fn quarantine_retry(&self, id: i64) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let moved = tx.execute(
            "INSERT INTO outbox (span_json, created_unix_nano, attempts)
            SELECT span_json, strftime('%s','now') * 1000000000, 0 FROM quarantine WHERE id = ?1",
            [id],
        )?;
        tx.execute("DELETE FROM quarantine WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(moved == 1)
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
mod quarantine_tests {
    use crate::Store;
    #[test]
    fn summaries_are_bounded_payload_free_and_retry_is_transactional() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        for _ in 0..3 {
            store
                .outbox_enqueue("synthetic-private-envelope-canary")
                .unwrap();
        }
        let ids: Vec<_> = store
            .outbox_take_batch(3)
            .unwrap()
            .iter()
            .map(|(id, _)| *id)
            .collect();
        store
            .outbox_quarantine(&ids, "synthetic-private-reason-canary")
            .unwrap();
        let first = store.quarantine_summaries(0, 1).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].category, "other");
        assert!(!format!("{first:?}").contains("canary"));
        let rest = store.quarantine_summaries(first[0].id, 100).unwrap();
        assert_eq!(rest.len(), 2);
        store.conn.execute_batch("CREATE TRIGGER synthetic_failure BEFORE INSERT ON outbox BEGIN SELECT RAISE(FAIL, 'synthetic'); END;").unwrap();
        assert!(store.quarantine_retry(first[0].id).is_err());
        assert_eq!(store.quarantine_len().unwrap(), 3);
        store
            .conn
            .execute_batch("DROP TRIGGER synthetic_failure")
            .unwrap();
        assert!(store.quarantine_retry(first[0].id).unwrap());
        assert!(!store.quarantine_retry(first[0].id).unwrap());
        assert_eq!(store.outbox_len().unwrap(), 1);
        assert_eq!(store.quarantine_len().unwrap(), 2);
        assert_eq!(
            store.outbox_take_batch(1).unwrap()[0].1,
            "synthetic-private-envelope-canary"
        );
    }
}
