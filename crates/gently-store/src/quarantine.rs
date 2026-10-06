//! Bounded metadata quarantine inspection and transactional retry.
use crate::{Result, Store};

/// Why an envelope left the active queue. The store owns the persisted
/// encoding, so callers never write free-form reason text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuarantineReason {
    /// The queued bytes are not a valid OTLP/JSON envelope.
    InvalidJson,
    /// The collector rejected the envelope as unprocessable.
    CollectorRejection { status: u16 },
}

// Persisted text. Binaries released before the typed reason wrote these exact
// strings, and they may still read the same database, so changing them needs a
// schema migration rather than an edit here.
const INVALID_JSON: &str = "invalid queued OTLP envelope JSON";
const REJECTION_PREFIX: &str = "collector rejected request (HTTP ";
const REJECTION_SUFFIX: &str = ")";

impl QuarantineReason {
    /// Stable short code for operator output.
    pub fn code(self) -> &'static str {
        match self {
            QuarantineReason::InvalidJson => "invalid_json",
            QuarantineReason::CollectorRejection { .. } => "collector_rejection",
        }
    }

    /// The collector's HTTP status, when the collector made the decision.
    pub fn http_status(self) -> Option<u16> {
        match self {
            QuarantineReason::InvalidJson => None,
            QuarantineReason::CollectorRejection { status } => Some(status),
        }
    }

    pub(crate) fn encode(self) -> String {
        match self {
            QuarantineReason::InvalidJson => INVALID_JSON.to_owned(),
            QuarantineReason::CollectorRejection { status } => {
                format!("{REJECTION_PREFIX}{status}{REJECTION_SUFFIX}")
            }
        }
    }

    /// `None` for text outside the typed encoding; the text itself is dropped.
    pub(crate) fn decode(stored: &str) -> Option<Self> {
        if stored == INVALID_JSON {
            return Some(QuarantineReason::InvalidJson);
        }
        let status = stored
            .strip_prefix(REJECTION_PREFIX)?
            .strip_suffix(REJECTION_SUFFIX)?
            .parse()
            .ok()?;
        Some(QuarantineReason::CollectorRejection { status })
    }
}

/// Bounded operational summary. Never exposes queued JSON or stored reason text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuarantineSummary {
    pub id: i64,
    pub bytes: u64,
    pub quarantined_unix_nano: u64,
    /// `None` when the stored reason is not one this build writes.
    pub reason: Option<QuarantineReason>,
}

impl Store {
    pub fn quarantine_summaries(
        &self,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<QuarantineSummary>> {
        let mut query = self.conn.prepare(
            "SELECT id, length(CAST(span_json AS BLOB)), quarantined_unix_nano, reason
            FROM quarantine WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = query
            .query_map(
                rusqlite::params![after_id.max(0), limit.clamp(1, 1000) as i64],
                |r| {
                    Ok(QuarantineSummary {
                        id: r.get(0)?,
                        bytes: r.get::<_, i64>(1)? as u64,
                        quarantined_unix_nano: r.get::<_, i64>(2)? as u64,
                        reason: QuarantineReason::decode(&r.get::<_, String>(3)?),
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
    use super::QuarantineReason;
    use crate::Store;

    fn quarantined(count: usize, reason: QuarantineReason) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        for _ in 0..count {
            store
                .outbox_enqueue("synthetic-private-envelope-canary")
                .unwrap();
        }
        let ids: Vec<_> = store
            .outbox_take_batch(count)
            .unwrap()
            .iter()
            .map(|(id, _)| *id)
            .collect();
        store.outbox_quarantine(&ids, reason).unwrap();
        (dir, store)
    }

    #[test]
    fn reasons_round_trip_through_the_persisted_encoding() {
        for reason in [
            QuarantineReason::InvalidJson,
            QuarantineReason::CollectorRejection { status: 409 },
            QuarantineReason::CollectorRejection { status: 413 },
        ] {
            assert_eq!(QuarantineReason::decode(&reason.encode()), Some(reason));
        }
        assert_eq!(QuarantineReason::InvalidJson.code(), "invalid_json");
        assert_eq!(QuarantineReason::InvalidJson.http_status(), None);
        let rejected = QuarantineReason::CollectorRejection { status: 422 };
        assert_eq!(rejected.code(), "collector_rejection");
        assert_eq!(rejected.http_status(), Some(422));
    }

    #[test]
    fn rows_written_by_earlier_releases_keep_their_category() {
        assert_eq!(
            QuarantineReason::decode("invalid queued OTLP envelope JSON"),
            Some(QuarantineReason::InvalidJson)
        );
        assert_eq!(
            QuarantineReason::decode("collector rejected request (HTTP 413)"),
            Some(QuarantineReason::CollectorRejection { status: 413 })
        );
        for unknown in [
            "synthetic-private-reason-canary",
            "collector rejected request (HTTP 99999)",
            "collector rejected request (HTTP 413) trailing",
        ] {
            assert_eq!(QuarantineReason::decode(unknown), None);
        }
    }

    #[test]
    fn summaries_are_bounded_typed_and_payload_free() {
        let (_dir, store) = quarantined(3, QuarantineReason::CollectorRejection { status: 413 });
        let first = store.quarantine_summaries(0, 1).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].reason,
            Some(QuarantineReason::CollectorRejection { status: 413 })
        );
        assert_eq!(
            first[0].bytes,
            "synthetic-private-envelope-canary".len() as u64
        );
        assert!(!format!("{first:?}").contains("canary"));
        let rest = store.quarantine_summaries(first[0].id, 100).unwrap();
        assert_eq!(rest.len(), 2);

        store
            .conn
            .execute(
                "UPDATE quarantine SET reason = 'synthetic-private-reason-canary' WHERE id = ?1",
                [rest[1].id],
            )
            .unwrap();
        let unknown = store.quarantine_summaries(rest[0].id, 100).unwrap();
        assert_eq!(unknown[0].reason, None);
        assert!(!format!("{unknown:?}").contains("canary"));
    }

    #[test]
    fn retry_rolls_back_when_the_quarantine_delete_fails() {
        let (_dir, store) = quarantined(3, QuarantineReason::InvalidJson);
        let id = store.quarantine_summaries(0, 1).unwrap()[0].id;
        // The INSERT into outbox succeeds; only the DELETE that follows fails.
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER synthetic_failure BEFORE DELETE ON quarantine
                 BEGIN SELECT RAISE(FAIL, 'synthetic'); END;",
            )
            .unwrap();
        assert!(store.quarantine_retry(id).is_err());
        assert_eq!(store.outbox_len().unwrap(), 0, "requeue rolled back");
        assert_eq!(store.quarantine_len().unwrap(), 3, "row still retained");

        store
            .conn
            .execute_batch("DROP TRIGGER synthetic_failure")
            .unwrap();
        assert!(store.quarantine_retry(id).unwrap());
        assert!(!store.quarantine_retry(id).unwrap());
        assert_eq!(store.outbox_len().unwrap(), 1);
        assert_eq!(store.quarantine_len().unwrap(), 2);
        assert_eq!(
            store.outbox_take_batch(1).unwrap()[0].1,
            "synthetic-private-envelope-canary"
        );
    }
}
