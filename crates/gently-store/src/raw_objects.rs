//! Immutable tenant-scoped ciphertext storage and the ciphertext upload queue.

use crate::{Result, Store, StoreError};
use gently_raw::RawObject;
use rusqlite::OptionalExtension;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawObjectStats {
    pub objects: usize,
    pub bytes: u64,
    pub pending_objects: usize,
    pub pending_bytes: u64,
}

impl Store {
    /// Retain locally captured ciphertext until an exporter acknowledges it.
    pub fn raw_object_put(&self, object: &RawObject) -> Result<()> {
        self.insert_raw_object(object, false)
    }

    /// Cache remotely fetched ciphertext without scheduling another upload.
    pub fn raw_object_cache(&self, object: &RawObject) -> Result<()> {
        self.insert_raw_object(object, true)
    }

    fn insert_raw_object(&self, object: &RawObject, synced: bool) -> Result<()> {
        self.insert_raw_object_with_budget(object, synced, crate::RAW_OBJECT_CAP_BYTES)
    }

    fn insert_raw_object_with_budget(
        &self,
        object: &RawObject,
        synced: bool,
        budget: usize,
    ) -> Result<()> {
        object
            .validate()
            .map_err(|_| StoreError::InvalidRawObject)?;
        let json = serde_json::to_string(object).map_err(|_| StoreError::InvalidRawObject)?;
        if let Some(existing) =
            self.raw_object_get(&object.context.tenant_id, &object.context.raw_ref)?
        {
            return if existing == *object {
                Ok(())
            } else {
                Err(StoreError::RawObjectConflict)
            };
        }
        // Admission and insertion share one SQLite write statement so concurrent
        // hooks cannot each spend the same remaining capacity.
        let inserted = self.conn.execute(
            "INSERT INTO raw_objects (tenant_id, raw_ref, object_json, created_unix_nano, synced)
             SELECT ?1, ?2, ?3, strftime('%s','now') * 1000000000, ?4
             WHERE (SELECT COALESCE(SUM(length(CAST(object_json AS BLOB))), 0) FROM raw_objects) + ?5 <= ?6
             ON CONFLICT(tenant_id, raw_ref) DO NOTHING",
            rusqlite::params![
                object.context.tenant_id,
                object.context.raw_ref,
                json,
                synced,
                json.len() as i64,
                budget as i64
            ],
        )?;
        if inserted == 1 {
            return Ok(());
        }
        match self.raw_object_get(&object.context.tenant_id, &object.context.raw_ref)? {
            Some(existing) if existing == *object => Ok(()),
            Some(_) => Err(StoreError::RawObjectConflict),
            None => Err(StoreError::RawCapacity),
        }
    }

    pub fn raw_object_get(&self, tenant_id: &str, raw_ref: &str) -> Result<Option<RawObject>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT object_json FROM raw_objects WHERE tenant_id = ?1 AND raw_ref = ?2",
                rusqlite::params![tenant_id, raw_ref],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|json| decode_object(&json, tenant_id, raw_ref))
            .transpose()
    }

    /// Read pending objects without consuming them; failed uploads retry these
    /// exact bytes and references.
    pub fn raw_objects_pending(&self, tenant_id: &str, limit: usize) -> Result<Vec<RawObject>> {
        let mut stmt = self.conn.prepare(
            "SELECT raw_ref, object_json FROM raw_objects WHERE tenant_id = ?1 AND synced = 0
             ORDER BY created_unix_nano, raw_ref LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![tenant_id, limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(raw_ref, json)| decode_object(&json, tenant_id, &raw_ref))
            .collect()
    }

    /// Acknowledge only the authenticated tenant's successfully uploaded refs.
    pub fn raw_objects_mark_synced(&self, tenant_id: &str, raw_refs: &[String]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for raw_ref in raw_refs {
            tx.execute(
                "UPDATE raw_objects SET synced = 1 WHERE tenant_id = ?1 AND raw_ref = ?2",
                rusqlite::params![tenant_id, raw_ref],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn raw_objects_len(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT count(*) FROM raw_objects", [], |r| r.get(0))?;
        Ok(count as usize)
    }

    /// Encoded ciphertext object bytes; excludes SQLite page and WAL overhead.
    pub fn raw_objects_stats(&self, tenant_id: &str) -> Result<RawObjectStats> {
        self.conn.query_row(
            "SELECT count(*), COALESCE(SUM(length(CAST(object_json AS BLOB))), 0),
                    COALESCE(SUM(CASE WHEN synced = 0 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN synced = 0 THEN length(CAST(object_json AS BLOB)) ELSE 0 END), 0)
             FROM raw_objects WHERE tenant_id = ?1",
            [tenant_id], |row| Ok(RawObjectStats {
                objects: row.get::<_, i64>(0)? as usize,
                bytes: row.get::<_, i64>(1)? as u64,
                pending_objects: row.get::<_, i64>(2)? as usize,
                pending_bytes: row.get::<_, i64>(3)? as u64,
            }),
        ).map_err(Into::into)
    }
}

fn decode_object(json: &str, tenant_id: &str, raw_ref: &str) -> Result<RawObject> {
    let object: RawObject = serde_json::from_str(json).map_err(|_| StoreError::InvalidRawObject)?;
    object
        .validate()
        .map_err(|_| StoreError::InvalidRawObject)?;
    if object.context.tenant_id != tenant_id || object.context.raw_ref != raw_ref {
        return Err(StoreError::InvalidRawObject);
    }
    Ok(object)
}

#[cfg(test)]
mod tests {
    use crate::Store;
    use gently_raw::{
        DeviceIdentity, Manifest, OwnerKey, RawContext, RawObject, Recipient, TrustPin,
        VerifiedManifest,
    };
    use std::collections::BTreeMap;

    fn object(tenant: &str, raw_ref: &str, value: &str) -> RawObject {
        let identity = DeviceIdentity::generate();
        let owner = OwnerKey::generate();
        let manifest = Manifest {
            version: 1,
            tenant_id: tenant.into(),
            key_epoch: 1,
            expires_unix_secs: 4_102_444_800,
            readers: vec![Recipient {
                device_id: "reader".into(),
                key_id: "key".into(),
                recipient: identity.to_public().to_string(),
            }],
        };
        let pin = TrustPin {
            tenant_id: tenant.into(),
            owner_verify_key_b64: owner.verification_key_b64(),
            min_epoch: 1,
            manifest_digest: gently_raw::manifest_digest(&manifest).unwrap(),
        };
        let verified =
            VerifiedManifest::verify(&gently_raw::sign_manifest(manifest, &owner).unwrap(), &pin)
                .unwrap();
        gently_raw::seal(
            &verified,
            RawContext {
                tenant_id: tenant.into(),
                device_id: "writer".into(),
                key_epoch: 1,
                raw_ref: raw_ref.into(),
                session_id: "session".into(),
                harness: "codex".into(),
                event: "UserPromptSubmit".into(),
            },
            BTreeMap::from([("gently.prompt".into(), value.into())]),
            BTreeMap::from([("gently.prompt".into(), vec!["0123456789abcdef".into()])]),
        )
        .unwrap()
    }

    #[test]
    fn raw_objects_are_tenant_scoped_immutable_and_ciphertext_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let store = Store::open(&path).unwrap();
        let reference = "0123456789abcdef0123456789abcdef";
        let first = object("personal", reference, "private canary fixture");
        store.raw_object_put(&first).unwrap();
        store.raw_object_put(&first).unwrap();
        assert_eq!(store.raw_objects_len().unwrap(), 1);
        assert_eq!(
            store.raw_object_get("personal", reference).unwrap(),
            Some(first.clone())
        );
        assert!(store.raw_object_get("other", reference).unwrap().is_none());
        let conflict = object("personal", reference, "different fixture");
        assert!(store.raw_object_put(&conflict).is_err());
        store
            .raw_object_put(&object("other", reference, "other tenant fixture"))
            .unwrap();
        assert_eq!(store.raw_objects_len().unwrap(), 2);
        for suffix in ["", "-wal"] {
            let bytes = std::fs::read(format!("{}{suffix}", path.display())).unwrap();
            assert!(!bytes
                .windows(b"private canary fixture".len())
                .any(|w| w == b"private canary fixture"));
        }
    }

    #[test]
    fn ciphertext_upload_queue_retries_and_remote_cache_is_not_requeued() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        let first = object("personal", "00000000000000000000000000000001", "fixture");
        let cached = object(
            "personal",
            "00000000000000000000000000000002",
            "remote fixture",
        );
        store.raw_object_put(&first).unwrap();
        store.raw_object_cache(&cached).unwrap();
        assert_eq!(
            store.raw_objects_pending("personal", 10).unwrap(),
            vec![first.clone()]
        );
        assert_eq!(
            store.raw_objects_pending("personal", 10).unwrap(),
            vec![first.clone()]
        );
        store
            .raw_objects_mark_synced("other", std::slice::from_ref(&first.context.raw_ref))
            .unwrap();
        assert_eq!(store.raw_objects_pending("personal", 10).unwrap().len(), 1);
        store
            .raw_objects_mark_synced("personal", std::slice::from_ref(&first.context.raw_ref))
            .unwrap();
        assert!(store
            .raw_objects_pending("personal", 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .raw_object_get("personal", &cached.context.raw_ref)
                .unwrap(),
            Some(cached)
        );
    }

    #[test]
    fn event_failure_rolls_back_ciphertext_and_spans_together() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        let object = object("personal", "00000000000000000000000000000001", "fixture");
        let result: crate::Result<()> = store.transaction(|store| {
            store.raw_object_put(&object)?;
            store.outbox_enqueue("synthetic envelope")?;
            Err(crate::StoreError::InvalidRawObject)
        });
        assert!(result.is_err());
        assert_eq!(store.raw_objects_len().unwrap(), 0);
        assert_eq!(store.outbox_len().unwrap(), 0);
    }

    #[test]
    fn byte_budget_refuses_new_objects_without_evicting_or_overwriting_existing_ciphertext() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        let first = object(
            "personal",
            "00000000000000000000000000000001",
            "first fixture",
        );
        let second = object(
            "personal",
            "00000000000000000000000000000002",
            "second fixture",
        );
        let bytes = serde_json::to_vec(&first).unwrap().len();
        assert!(store
            .insert_raw_object_with_budget(&first, false, bytes - 1)
            .is_err());
        assert_eq!(store.raw_objects_len().unwrap(), 0);
        store
            .insert_raw_object_with_budget(&first, false, bytes)
            .unwrap();
        store
            .insert_raw_object_with_budget(&first, false, bytes)
            .unwrap();
        assert!(store
            .insert_raw_object_with_budget(&second, false, bytes)
            .is_err());
        assert!(store
            .insert_raw_object_with_budget(&second, true, bytes)
            .is_err());
        assert_eq!(
            store
                .raw_object_get("personal", &first.context.raw_ref)
                .unwrap(),
            Some(first)
        );
        assert_eq!(store.raw_objects_len().unwrap(), 1);
        let stats = store.raw_objects_stats("personal").unwrap();
        assert_eq!(stats.objects, 1);
        assert_eq!(stats.pending_objects, 1);
        assert_eq!(stats.bytes, bytes as u64);
        assert_eq!(stats.pending_bytes, bytes as u64);
        store
            .raw_objects_mark_synced("personal", &["00000000000000000000000000000001".into()])
            .unwrap();
        let stats = store.raw_objects_stats("personal").unwrap();
        assert_eq!(stats.objects, 1);
        assert_eq!(stats.pending_objects, 0);
        assert_eq!(stats.bytes, bytes as u64);
        assert_eq!(stats.pending_bytes, 0);
        assert_eq!(store.raw_objects_stats("other").unwrap().bytes, 0);
    }
}
