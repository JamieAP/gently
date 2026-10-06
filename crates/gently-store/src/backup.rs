//! Consistent SQLite backups; raw objects remain opaque ciphertext.
use crate::{private_fs, Result, Store, StoreError, SCHEMA_VERSION};
use rusqlite::{Connection, DatabaseName, OpenFlags};
use std::path::{Path, PathBuf};

impl Store {
    pub fn backup(&self, destination: &Path, tenant: &str, device: &str) -> Result<()> {
        validate_encrypted_schema(&self.conn)?;
        publish_database(destination, |temporary| {
            self.conn.backup(DatabaseName::Main, temporary, None)?;
            let copy = Connection::open(temporary)?;
            copy.pragma_update(None, "journal_mode", "DELETE")?;
            copy.execute_batch(
                "CREATE TABLE IF NOT EXISTS backup_metadata (
                id INTEGER PRIMARY KEY CHECK(id = 1), version INTEGER NOT NULL,
                tenant_id TEXT NOT NULL, device_id TEXT NOT NULL);",
            )?;
            copy.execute(
                "INSERT INTO backup_metadata VALUES (1, 1, ?1, ?2)
                ON CONFLICT(id) DO UPDATE SET version=1, tenant_id=?1, device_id=?2",
                [tenant, device],
            )?;
            Ok(())
        })
    }

    /// Restore only a pinned backup, into a path that does not exist. Publishing
    /// uses exclusive hard-link creation so a concurrent new DB is never replaced.
    pub fn restore_backup(
        source: &Path,
        destination: &Path,
        tenant: &str,
        device: &str,
    ) -> Result<()> {
        for path in sqlite_paths(source) {
            private_fs::harden_existing_file(&path)?;
        }
        let source = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        validate_encrypted_schema(&source)?;
        let namespace = source
            .query_row(
                "SELECT version, tenant_id, device_id FROM backup_metadata WHERE id=1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|_| StoreError::InvalidBackup)?;
        if namespace.0 != 1 {
            return Err(StoreError::InvalidBackup);
        }
        if namespace.1 != tenant || namespace.2 != device {
            return Err(StoreError::BackupNamespace);
        }
        let integrity: String = source.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(StoreError::InvalidBackup);
        }
        publish_database(destination, |temporary| {
            source.backup(DatabaseName::Main, temporary, None)?;
            let copy = Connection::open(temporary)?;
            copy.pragma_update(None, "journal_mode", "DELETE")?;
            Ok(())
        })
    }
}

fn validate_encrypted_schema(connection: &Connection) -> Result<()> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let valid: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='raw_objects')
        AND NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name='raw_values')",
        [],
        |row| row.get(0),
    )?;
    match (version, valid) {
        (SCHEMA_VERSION, true) => Ok(()),
        _ => Err(StoreError::InvalidBackup),
    }
}

fn publish_database(destination: &Path, write: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    require_missing_database(destination)?;
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = parent.join(format!(".gently-recovery-{}.db", gently_raw::new_raw_ref()));
    let file = private_fs::create_private_file(&temporary)?;
    // Keep the precreated private handle open until SQLite finishes. Failure
    // removes only this operation's newly created temporary files.
    let outcome = write(&temporary).and_then(|()| {
        file.sync_all()?;
        require_missing_database(destination)?;
        std::fs::hard_link(&temporary, destination)?;
        Ok(())
    });
    drop(file);
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut name = temporary.as_os_str().to_os_string();
        name.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(name));
    }
    outcome?;
    private_fs::harden_existing_file(destination)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn sqlite_paths(path: &Path) -> Vec<PathBuf> {
    ["", "-wal", "-shm", "-journal"]
        .into_iter()
        .map(|suffix| {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            PathBuf::from(name)
        })
        .collect()
}

fn require_missing_database(path: &Path) -> Result<()> {
    for candidate in sqlite_paths(path) {
        match std::fs::symlink_metadata(candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "database destination or sidecar already exists",
                )
                .into())
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_wal_backup_preserves_queue_quarantine_and_open_state() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let store = Store::open(&source).unwrap();
        store
            .conn
            .pragma_update(None, "wal_autocheckpoint", 0)
            .unwrap();
        store.outbox_enqueue("synthetic metadata one").unwrap();
        store.outbox_enqueue("synthetic metadata two").unwrap();
        let id = store.outbox_take_batch(1).unwrap()[0].0;
        store.outbox_quarantine(&[id], "synthetic reason").unwrap();
        store.next_turn_index("synthetic-session").unwrap();
        let backup = dir.path().join("backup.db");
        store
            .backup(&backup, "synthetic-tenant", "synthetic-device")
            .unwrap();
        let restored = dir.path().join("restored.db");
        Store::restore_backup(&backup, &restored, "synthetic-tenant", "synthetic-device").unwrap();
        let recovered = Store::open(&restored).unwrap();
        assert_eq!(recovered.outbox_len().unwrap(), 1);
        assert_eq!(recovered.quarantine_len().unwrap(), 1);
        assert_eq!(recovered.next_turn_index("synthetic-session").unwrap(), 2);
        assert!(
            Store::restore_backup(&backup, &restored, "synthetic-tenant", "synthetic-device")
                .is_err()
        );
        assert_eq!(recovered.outbox_len().unwrap(), 1);
        assert!(matches!(
            Store::restore_backup(
                &backup,
                &dir.path().join("wrong.db"),
                "other",
                "synthetic-device"
            ),
            Err(StoreError::BackupNamespace)
        ));
        assert!(!dir.path().join("wrong.db").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(backup).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn rejects_plaintext_schema_and_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let store = Store::open(&source).unwrap();
        let dest = dir.path().join("retained.db");
        std::fs::write(&dest, b"synthetic existing artifact").unwrap();
        assert!(store.backup(&dest, "synthetic", "synthetic").is_err());
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"synthetic existing artifact"
        );
        store
            .conn
            .execute_batch("CREATE TABLE raw_values (plaintext TEXT)")
            .unwrap();
        assert!(matches!(
            store.backup(&dir.path().join("refused.db"), "synthetic", "synthetic"),
            Err(StoreError::InvalidBackup)
        ));
        assert!(!dir.path().join("refused.db").exists());
    }
    #[test]
    fn publication_refuses_orphan_sidecars_and_preserves_them() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("source.db")).unwrap();
        let backup = dir.path().join("backup.db");
        store.backup(&backup, "synthetic", "synthetic").unwrap();
        for suffix in ["-wal", "-shm", "-journal"] {
            let dest = dir.path().join(format!("dest{suffix}.db"));
            let sidecar = PathBuf::from(format!("{}{suffix}", dest.display()));
            std::fs::write(&sidecar, b"synthetic orphan sidecar").unwrap();
            assert!(store.backup(&dest, "synthetic", "synthetic").is_err());
            assert!(Store::restore_backup(&backup, &dest, "synthetic", "synthetic").is_err());
            assert!(!dest.exists());
            assert_eq!(std::fs::read(sidecar).unwrap(), b"synthetic orphan sidecar");
        }
        #[cfg(unix)]
        {
            let dest = dir.path().join("dangling.db");
            let sidecar = dir.path().join("dangling.db-journal");
            std::os::unix::fs::symlink(dir.path().join("missing"), &sidecar).unwrap();
            assert!(Store::restore_backup(&backup, &dest, "synthetic", "synthetic").is_err());
            assert!(!dest.exists());
            assert!(std::fs::symlink_metadata(sidecar)
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }
    #[test]
    fn backup_retains_opaque_raw_objects_and_reader_can_decrypt_after_restore() {
        use gently_raw::{
            DeviceIdentity, Manifest, OwnerKey, RawContext, ReaderIdentities, Recipient, TrustPin,
            VerifiedManifest,
        };
        use std::collections::BTreeMap;
        let identity = DeviceIdentity::generate();
        let owner = OwnerKey::generate();
        let manifest = Manifest {
            version: 1,
            tenant_id: "synthetic".into(),
            key_epoch: 1,
            expires_unix_secs: 4_102_444_800,
            readers: vec![Recipient {
                device_id: "reader".into(),
                key_id: "key".into(),
                recipient: identity.to_public().to_string(),
            }],
        };
        let pin = TrustPin {
            tenant_id: "synthetic".into(),
            owner_verify_key_b64: owner.verification_key_b64(),
            min_epoch: 1,
            manifest_digest: gently_raw::manifest_digest(&manifest).unwrap(),
        };
        let verified =
            VerifiedManifest::verify(&gently_raw::sign_manifest(manifest, &owner).unwrap(), &pin)
                .unwrap();
        let context = RawContext {
            tenant_id: "synthetic".into(),
            device_id: "writer".into(),
            key_epoch: 1,
            raw_ref: "0123456789abcdef0123456789abcdef".into(),
            session_id: "synthetic-session".into(),
            harness: "codex".into(),
            event: "UserPromptSubmit".into(),
        };
        let canary = "synthetic-backup-raw-canary";
        let object = gently_raw::seal(
            &verified,
            context.clone(),
            BTreeMap::from([("gently.prompt".into(), canary.into())]),
            BTreeMap::from([("gently.prompt".into(), vec!["0123456789abcdef".into()])]),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("source.db")).unwrap();
        store.raw_object_put(&object).unwrap();
        store
            .raw_object_quarantine("synthetic", &context.raw_ref, 409)
            .unwrap();
        let backup = dir.path().join("backup.db");
        store.backup(&backup, "synthetic", "writer").unwrap();
        assert!(!std::fs::read(&backup)
            .unwrap()
            .windows(canary.len())
            .any(|bytes| bytes == canary.as_bytes()));
        let dest = dir.path().join("restored.db");
        Store::restore_backup(&backup, &dest, "synthetic", "writer").unwrap();
        let restored = Store::open(&dest).unwrap();
        let recovered = restored
            .raw_object_get("synthetic", &context.raw_ref)
            .unwrap()
            .unwrap();
        assert_eq!(recovered, object);
        assert_eq!(
            restored
                .raw_objects_stats("synthetic")
                .unwrap()
                .quarantined_objects,
            1
        );
        let raw = gently_raw::open(
            &recovered,
            &context,
            &ReaderIdentities::from_native(vec![identity]),
        )
        .unwrap();
        assert_eq!(raw.fields["gently.prompt"], canary);
    }
}
