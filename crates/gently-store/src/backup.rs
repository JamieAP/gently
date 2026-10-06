//! Consistent SQLite backups; raw objects remain opaque ciphertext.
use crate::{private_fs, Result, Store, StoreError, SCHEMA_VERSION};
use rusqlite::{
    backup::{Backup, StepResult},
    Connection, OpenFlags,
};
use std::path::{Path, PathBuf};

impl Store {
    pub fn backup(&self, destination: &Path, tenant: &str, device: &str) -> Result<()> {
        validate_encrypted_schema(&self.conn)?;
        publish_database(destination, |temporary| {
            let copy = snapshot(&self.conn, temporary)?;
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
            let copy = snapshot(&source, temporary)?;
            copy.pragma_update(None, "journal_mode", "DELETE")?;
            Ok(())
        })
    }
}

// One step holds a single read snapshot. Incremental steps can restart forever
// when another connection commits between them. Busy locks fail for a retry.
fn snapshot(source: &Connection, destination: &Path) -> Result<Connection> {
    let mut copy = Connection::open(destination)?;
    copy.busy_timeout(std::time::Duration::from_secs(2))?;
    {
        let backup = Backup::new(source, &mut copy)?;
        if backup.step(-1)? != StepResult::Done {
            return Err(StoreError::BackupBusy);
        }
    }
    Ok(copy)
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

const TEMPORARY_PREFIX: &str = ".gently-recovery-";
const SQLITE_SUFFIXES: [&str; 4] = ["", "-wal", "-shm", "-journal"];

fn directory_of(destination: &Path) -> &Path {
    destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// A file beside a backup destination that is named like the private partial
/// copy an interrupted backup leaves behind.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Leftover {
    /// Exact temporary name, and a regular single-link owner-only file owned
    /// by this user: only this tool creates such a file.
    Ours(PathBuf),
    /// Shares the prefix but cannot be proven ours; it is only ever reported.
    Unverified(PathBuf),
}

#[derive(Debug, PartialEq, Eq)]
enum TemporaryName {
    Unrelated,
    Exact,
    Prefixed,
}

/// `.gently-recovery-<32 lowercase hex>.db` plus an optional SQLite sidecar.
fn temporary_name(name: &str) -> TemporaryName {
    let Some(rest) = name.strip_prefix(TEMPORARY_PREFIX) else {
        return TemporaryName::Unrelated;
    };
    let exact = rest.split_once(".db").is_some_and(|(id, suffix)| {
        id.len() == 32
            && id
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            && SQLITE_SUFFIXES.contains(&suffix)
    });
    match exact {
        true => TemporaryName::Exact,
        false => TemporaryName::Prefixed,
    }
}

fn private_regular_file(path: &Path) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    #[cfg(unix)]
    let private = {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        metadata.uid() == unsafe { libc::geteuid() }
            && metadata.nlink() == 1
            && metadata.permissions().mode() & 0o077 == 0
    };
    // Ownership cannot be proven here, so nothing is ever classed as ours.
    #[cfg(not(unix))]
    let private = false;
    Ok(metadata.file_type().is_file() && private)
}

/// List interrupted-backup temporaries beside `destination`. Read-only; a
/// missing directory has none.
pub fn recovery_leftovers(destination: &Path) -> Result<Vec<Leftover>> {
    let directory = directory_of(destination);
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut leftovers = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = directory.join(entry.file_name());
        match temporary_name(&entry.file_name().to_string_lossy()) {
            TemporaryName::Unrelated => (),
            TemporaryName::Exact if private_regular_file(&path)? => {
                leftovers.push(Leftover::Ours(path))
            }
            TemporaryName::Exact | TemporaryName::Prefixed => {
                leftovers.push(Leftover::Unverified(path))
            }
        }
    }
    leftovers.sort();
    Ok(leftovers)
}

/// Remove only leftovers proven ours, re-checking each just before unlinking.
/// Returns how many were removed; unverified entries are never touched.
pub fn remove_recovery_leftovers(leftovers: &[Leftover]) -> Result<usize> {
    let mut removed = 0;
    for leftover in leftovers {
        let Leftover::Ours(path) = leftover else {
            continue;
        };
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if temporary_name(&name) != TemporaryName::Exact || !private_regular_file(path)? {
            continue;
        }
        match std::fs::remove_file(path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(removed)
}

fn publish_database(destination: &Path, write: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    require_missing_database(destination)?;
    let parent = directory_of(destination);
    let temporary = parent.join(format!(
        "{TEMPORARY_PREFIX}{}.db",
        gently_raw::new_raw_ref()
    ));
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
    for path in sqlite_paths(&temporary) {
        let _ = std::fs::remove_file(path);
    }
    outcome?;
    private_fs::harden_existing_file(destination)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn sqlite_paths(path: &Path) -> Vec<PathBuf> {
    SQLITE_SUFFIXES
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
    fn backup_completes_during_continuous_writes_from_another_connection() {
        use std::{
            sync::{
                atomic::{AtomicBool, Ordering},
                Arc,
            },
            time::{Duration, Instant},
        };
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let store = Store::open(&source).unwrap();
        store.conn.execute_batch("CREATE TABLE synthetic_backup_fixture (id INTEGER PRIMARY KEY, payload BLOB); WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<1024) INSERT INTO synthetic_backup_fixture SELECT n, zeroblob(16384) FROM ids;").unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let (ready, started) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let writer = Connection::open(source).unwrap();
            writer.busy_timeout(Duration::from_secs(1)).unwrap();
            writer.execute("INSERT INTO outbox(span_json, created_unix_nano, attempts) VALUES ('synthetic concurrent write', 1, 0)", []).unwrap();
            ready.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(4);
            while !stopped.load(Ordering::Relaxed) && Instant::now() < deadline {
                writer
                    .execute(
                        "UPDATE outbox SET created_unix_nano=created_unix_nano+1",
                        [],
                    )
                    .unwrap();
            }
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let began = Instant::now();
        let backup = dir.path().join("backup.db");
        let result = store.backup(&backup, "synthetic", "writer");
        let elapsed = began.elapsed();
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        result.unwrap();
        assert!(
            elapsed < Duration::from_secs(2),
            "backup did not finish while writer remained active"
        );
        let copy = Connection::open(backup).unwrap();
        assert_eq!(
            copy.query_row("SELECT COUNT(*) FROM synthetic_backup_fixture", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            1024
        );
        assert_eq!(
            copy.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }

    #[test]
    fn busy_snapshot_fails_for_retry_without_publishing_or_leaving_temporaries() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("source.db")).unwrap();
        // SQLite refuses to snapshot a source that is mid-write.
        store.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let backup = dir.path().join("backup.db");
        assert!(matches!(
            store.backup(&backup, "synthetic", "synthetic"),
            Err(StoreError::BackupBusy)
        ));
        for path in sqlite_paths(&backup) {
            assert!(std::fs::symlink_metadata(path).is_err());
        }
        assert_eq!(recovery_leftovers(&backup).unwrap(), Vec::new());
        store.conn.execute_batch("ROLLBACK").unwrap();
        store.backup(&backup, "synthetic", "synthetic").unwrap();
    }

    #[test]
    fn temporary_names_match_only_the_exact_private_pattern() {
        let id = "0123456789abcdef0123456789abcdef";
        for suffix in SQLITE_SUFFIXES {
            assert_eq!(
                temporary_name(&format!(".gently-recovery-{id}.db{suffix}")),
                TemporaryName::Exact
            );
        }
        for prefixed in [
            ".gently-recovery-notes.txt".to_string(),
            format!(".gently-recovery-{}.db", id.to_uppercase()),
            format!(".gently-recovery-{id}0.db"),
            format!(".gently-recovery-{id}.db-other"),
            format!(".gently-recovery-{id}.db.bak"),
        ] {
            assert_eq!(temporary_name(&prefixed), TemporaryName::Prefixed);
        }
        for unrelated in ["backup.db", "gently-recovery-x.db", ".gently-state.db"] {
            assert_eq!(temporary_name(unrelated), TemporaryName::Unrelated);
        }
    }

    #[cfg(unix)]
    #[test]
    fn leftovers_report_lookalikes_and_remove_only_exact_private_temporaries() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let destination = dir.path().join("backup.db");
        let named = |id: char, suffix: &str| {
            dir.path().join(format!(
                ".gently-recovery-{}.db{suffix}",
                id.to_string().repeat(32)
            ))
        };
        let write = |path: &Path, mode: u32| {
            std::fs::write(path, b"synthetic partial copy").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let ours = [named('a', ""), named('a', "-journal")];
        for path in &ours {
            write(path, 0o600);
        }
        let readable = named('b', "");
        write(&readable, 0o644);
        let target = elsewhere.path().join("user-file");
        write(&target, 0o600);
        let symlinked = named('c', "");
        std::os::unix::fs::symlink(&target, &symlinked).unwrap();
        let hard_linked = named('d', "-wal");
        std::fs::hard_link(&target, &hard_linked).unwrap();
        let prefixed = dir.path().join(".gently-recovery-notes.txt");
        write(&prefixed, 0o600);
        let unrelated = dir.path().join("keep.db");
        write(&unrelated, 0o600);

        let leftovers = recovery_leftovers(&destination).unwrap();
        let mut expected = vec![
            Leftover::Ours(ours[0].clone()),
            Leftover::Ours(ours[1].clone()),
            Leftover::Unverified(readable.clone()),
            Leftover::Unverified(symlinked.clone()),
            Leftover::Unverified(hard_linked.clone()),
            Leftover::Unverified(prefixed.clone()),
        ];
        expected.sort();
        assert_eq!(leftovers, expected);

        assert_eq!(remove_recovery_leftovers(&leftovers).unwrap(), 2);
        for path in &ours {
            assert!(std::fs::symlink_metadata(path).is_err());
        }
        for kept in [&readable, &symlinked, &hard_linked, &prefixed, &unrelated] {
            assert!(std::fs::symlink_metadata(kept).is_ok());
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"synthetic partial copy");
        assert!(recovery_leftovers(&dir.path().join("missing/backup.db"))
            .unwrap()
            .is_empty());
    }

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
