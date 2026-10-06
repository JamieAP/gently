//! Bounded metadata quarantine inspection and transactional retry.
use crate::{Result, Store};

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
