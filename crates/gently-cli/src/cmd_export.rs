//! Drain the local outbox once, or keep one authenticated exporter running.
//!
//! A process-wide advisory lock prevents concurrent exporters. Successful
//! delivery deletes rows; credential failures preserve them and stop, while
//! transient failures back off. Collector ingestion is idempotent, so stopping
//! an in-flight send safely leaves its rows queued for the next exporter.

use crate::config::Config;
use anyhow::{Context, Result};
use fs4::fs_std::FileExt;
use gently_export::{drain, ExportError, Http2Transport, PreferQuic, QuicTransport, Transport};
use gently_store::Store;
use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_ATTEMPTS: u32 = 3;
const BACKOFF_BASE: Duration = Duration::from_millis(250);
const WATCH_MAX_BACKOFF: Duration = Duration::from_secs(30);

pub fn run() -> Result<()> {
    run_mode(None)
}

/// Unlock credentials once in a foreground launcher, then drain new hook rows.
pub fn watch(interval_secs: u64) -> Result<()> {
    run_mode(Some(Duration::from_secs(interval_secs.max(1))))
}

fn run_mode(watch_interval: Option<Duration>) -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    crate::logging::init_file_log(&cfg.runtime_dir().join("export.log"));
    cfg.require_collector()?;

    let lock_path = cfg.runtime_dir().join("export.lock");
    let lock = gently_store::private_fs::open_private_file(&lock_path, false)
        .with_context(|| format!("opening {}", lock_path.display()))?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            if watch_interval.is_some() {
                anyhow::bail!("another exporter is already running");
            }
            tracing::debug!("another exporter holds the lock; exiting");
            return Ok(());
        }
        Err(e) => return Err(e).context("acquiring export lock"),
    }
    let store = Store::open(&cfg.state_db())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    // Construct QUIC inside the runtime because reqwest spawns its driver.
    // The daemon keeps these clients and their connections for its lifetime.
    let result = runtime.block_on(async {
        let http2 = Http2Transport::new(
            &cfg.collector_url,
            &cfg.token,
            &cfg.tenant_id,
            cfg.export_timeout_secs,
        )?;
        let quic = if cfg.prefer_quic {
            match QuicTransport::new(
                &cfg.collector_url,
                &cfg.token,
                &cfg.tenant_id,
                cfg.export_timeout_secs,
            ) {
                Ok(q) => Some(q),
                Err(e) => {
                    tracing::warn!(error = %e, "QUIC client unavailable; using HTTP/2");
                    None
                }
            }
        } else {
            None
        };
        let transport = PreferQuic::new(quic, http2);
        let raw_sync = if cfg.sync_raw_values {
            Some(RawSync {
                client: crate::collector::CollectorClient::new(
                    &cfg.collector_url,
                    &cfg.token,
                    &cfg.tenant_id,
                    cfg.export_timeout_secs,
                )
                .map_err(|_| {
                    ExportError::Unavailable("building ciphertext client failed".into())
                })?,
                tenant_id: cfg.tenant_id.clone(),
            })
        } else {
            None
        };
        if let Some(interval) = watch_interval {
            tracing::info!(?interval, "export watcher started");
            watch_loop(
                &store,
                &transport,
                cfg.outbox_cap,
                cfg.export_batch,
                raw_sync.as_ref(),
                interval,
                tokio::signal::ctrl_c(),
            )
            .await
        } else {
            export_with_retry(
                &store,
                &transport,
                cfg.outbox_cap,
                cfg.export_batch,
                raw_sync.as_ref(),
            )
            .await
        }
    });
    let _ = FileExt::unlock(&lock);

    match &result {
        Ok(n) => {
            if watch_interval.is_some() {
                tracing::info!("export watcher stopped");
            } else {
                tracing::info!(delivered = n, "export drain complete");
                let _ = store.health_record_success(now_nanos());
            }
            Ok(())
        }
        Err(e) => {
            let _ = store.health_record_failure(now_nanos(), &e.to_string());
            Err(anyhow::anyhow!("{e}")).context("export drain")
        }
    }
}

async fn export_with_retry<T: Transport>(
    store: &Store,
    transport: &T,
    cap: usize,
    batch_size: usize,
    raw_sync: Option<&RawSync>,
) -> Result<usize, ExportError> {
    for attempt in 0..MAX_ATTEMPTS {
        match drain_all(store, transport, cap, batch_size, raw_sync).await {
            Ok(n) => return Ok(n),
            Err(e) => {
                if !e.retryable() || attempt + 1 == MAX_ATTEMPTS {
                    return Err(e);
                }
                let backoff = BACKOFF_BASE * 2u32.pow(attempt);
                tracing::warn!(error = %e, attempt = attempt + 1, ?backoff, "export retry");
                tokio::time::sleep(backoff).await;
            }
        }
    }
    unreachable!("retry budget is nonzero")
}

async fn watch_loop<T: Transport, F: Future<Output = std::io::Result<()>>>(
    store: &Store,
    transport: &T,
    cap: usize,
    batch_size: usize,
    raw_sync: Option<&RawSync>,
    interval: Duration,
    shutdown: F,
) -> Result<usize, ExportError> {
    tokio::pin!(shutdown);
    let mut failures = 0u32;
    let mut delivered_total = 0usize;
    loop {
        let mut delay = interval;
        if store.outbox_len()? > 0
            || raw_sync.is_some_and(|raw| {
                store
                    .raw_objects_pending(&raw.tenant_id, 1)
                    .map(|objects| !objects.is_empty())
                    .unwrap_or(true)
            })
        {
            let result = tokio::select! {
                stopped = &mut shutdown => {
                    stopped.map_err(|e| ExportError::Unavailable(format!("waiting for Ctrl-C: {e}")))?;
                    return Ok(delivered_total);
                }
                result = drain_all(store, transport, cap, batch_size, raw_sync) => result,
            };
            match result {
                Ok(n) => {
                    delivered_total += n;
                    failures = 0;
                    if n > 0 {
                        let _ = store.health_record_success(now_nanos());
                        tracing::info!(delivered = n, "export watcher drained outbox");
                    }
                }
                Err(e) => {
                    let _ = store.health_record_failure(now_nanos(), &e.to_string());
                    if !e.retryable() {
                        return Err(e);
                    }
                    delay = (BACKOFF_BASE * 2u32.pow(failures.min(7))).min(WATCH_MAX_BACKOFF);
                    failures = failures.saturating_add(1);
                    tracing::warn!(error = %e, ?delay, "export watcher unavailable; rows remain queued");
                }
            }
        }
        tokio::select! {
            stopped = &mut shutdown => {
                stopped.map_err(|e| ExportError::Unavailable(format!("waiting for Ctrl-C: {e}")))?;
                return Ok(delivered_total);
            }
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

struct RawSync {
    client: crate::collector::CollectorClient,
    tenant_id: String,
}

async fn drain_all<T: Transport>(
    store: &Store,
    transport: &T,
    cap: usize,
    batch_size: usize,
    raw_sync: Option<&RawSync>,
) -> Result<usize, ExportError> {
    if let Some(raw) = raw_sync {
        sync_ciphertext(store, &raw.client, &raw.tenant_id).await?;
    }
    drain(store, transport, cap, batch_size).await
}

async fn sync_ciphertext(
    store: &Store,
    client: &crate::collector::CollectorClient,
    tenant: &str,
) -> Result<usize, ExportError> {
    let mut delivered = 0;
    loop {
        let pending = store.raw_objects_pending(tenant, 16)?;
        if pending.is_empty() {
            return Ok(delivered);
        }
        for object in pending {
            client.upload_raw(&object).await?;
            store.raw_objects_mark_synced(tenant, &[object.context.raw_ref])?;
            delivered += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gently_core::{OtlpRequest, Resource};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn raw_fixture(store: &Store) -> gently_raw::RawObject {
        use gently_raw::*;
        use std::collections::BTreeMap;
        let identity = DeviceIdentity::generate();
        let owner = OwnerKey::generate();
        let manifest = Manifest {
            version: 1,
            tenant_id: "tenant-a".into(),
            key_epoch: 1,
            expires_unix_secs: 4_102_444_800,
            readers: vec![Recipient {
                device_id: "reader".into(),
                key_id: "reader-key".into(),
                recipient: identity.to_public().to_string(),
            }],
        };
        let signed = sign_manifest(manifest, &owner).unwrap();
        let pin = TrustPin {
            tenant_id: "tenant-a".into(),
            owner_verify_key_b64: owner.verification_key_b64(),
            min_epoch: 1,
            manifest_digest: manifest_digest(&signed.manifest).unwrap(),
        };
        let verified = VerifiedManifest::verify(&signed, &pin).unwrap();
        let context = RawContext {
            tenant_id: "tenant-a".into(),
            device_id: "capture-host".into(),
            key_epoch: 1,
            raw_ref: new_raw_ref(),
            session_id: "session".into(),
            harness: "codex".into(),
            event: "UserPromptSubmit".into(),
        };
        let object = seal(
            &verified,
            context,
            BTreeMap::from([("gently.prompt".into(), "synthetic-raw-canary".into())]),
            BTreeMap::from([("gently.prompt".into(), vec!["span".into()])]),
        )
        .unwrap();
        store.raw_object_put(&object).unwrap();
        object
    }

    #[tokio::test]
    async fn ciphertext_sync_needs_no_reader_key_and_acknowledges_only_success() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        let object = raw_fixture(&store);
        let (base, server) = crate::collector::tests::server("200 OK", "{}");
        let client =
            crate::collector::CollectorClient::new(&base, "synthetic-auth", "tenant-a", 2).unwrap();
        assert_eq!(
            sync_ciphertext(&store, &client, "tenant-a").await.unwrap(),
            1
        );
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /v1/raw-values?tenant_id=tenant-a "));
        assert!(request.contains(&object.ciphertext_b64));
        assert!(!request.contains("synthetic-raw-canary"));
        assert!(store
            .raw_objects_pending("tenant-a", 16)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn watch_uploads_ciphertext_when_metadata_outbox_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        raw_fixture(&store);
        let (base, server) = crate::collector::tests::server("200 OK", "{}");
        let raw = RawSync {
            client: crate::collector::CollectorClient::new(&base, "synthetic-auth", "tenant-a", 2)
                .unwrap(),
            tenant_id: "tenant-a".into(),
        };
        let transport = recorder(false, false);
        let shutdown = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(())
        };
        assert_eq!(
            watch_loop(
                &store,
                &transport,
                100,
                10,
                Some(&raw),
                Duration::from_millis(5),
                shutdown
            )
            .await
            .unwrap(),
            0
        );
        server.join().unwrap();
        assert!(store.raw_objects_pending("tenant-a", 1).unwrap().is_empty());
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rejected_ciphertext_authentication_preserves_pending_object() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        raw_fixture(&store);
        let (base, server) =
            crate::collector::tests::server("403 Forbidden", "synthetic-secret-never-forward");
        let client =
            crate::collector::CollectorClient::new(&base, "synthetic-auth", "tenant-a", 2).unwrap();
        assert!(matches!(
            sync_ciphertext(&store, &client, "tenant-a").await,
            Err(ExportError::Authentication(403))
        ));
        server.join().unwrap();
        assert_eq!(store.raw_objects_pending("tenant-a", 16).unwrap().len(), 1);
    }

    struct Recorder {
        calls: AtomicUsize,
        auth_failure: bool,
        unavailable: bool,
    }
    impl Transport for Recorder {
        async fn send(&self, _body: Vec<u8>) -> Result<(), ExportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.auth_failure {
                Err(ExportError::Authentication(401))
            } else if self.unavailable {
                Err(ExportError::Unavailable("synthetic unavailable".into()))
            } else {
                Ok(())
            }
        }
    }
    fn fixture() -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(&directory.path().join("state.db")).unwrap();
        enqueue(&store);
        (directory, store)
    }
    fn enqueue(store: &Store) {
        let request =
            OtlpRequest::single(&Resource::new("synthetic", "codex", "/synthetic"), vec![]);
        store
            .outbox_enqueue(&serde_json::to_string(&request).unwrap())
            .unwrap();
    }
    fn recorder(auth_failure: bool, unavailable: bool) -> Recorder {
        Recorder {
            calls: AtomicUsize::new(0),
            auth_failure,
            unavailable,
        }
    }

    #[tokio::test]
    async fn one_shot_authentication_failure_is_not_retried() {
        let (_directory, store) = fixture();
        let transport = recorder(true, false);
        assert!(matches!(
            export_with_retry(&store, &transport, 1000, 100, None).await,
            Err(ExportError::Authentication(401))
        ));
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.outbox_len().unwrap(), 1);
        assert_eq!(store.quarantine_len().unwrap(), 0);
    }

    #[tokio::test]
    async fn retry_budget_has_no_sleep_after_final_failure() {
        let (_directory, store) = fixture();
        let transport = recorder(false, true);
        let started = std::time::Instant::now();
        assert!(export_with_retry(&store, &transport, 1000, 100, None)
            .await
            .is_err());
        assert_eq!(transport.calls.load(Ordering::SeqCst), 3);
        assert_eq!(store.outbox_len().unwrap(), 1);
        // Two sleeps total 750ms; the old final sleep made this 1750ms.
        assert!(started.elapsed() < Duration::from_millis(1600));
    }

    #[tokio::test]
    async fn watch_drains_backlog_and_later_rows_then_stops_cleanly() {
        let (_directory, store) = fixture();
        let transport = recorder(false, false);
        let shutdown = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            enqueue(&store);
            tokio::time::sleep(Duration::from_millis(40)).await;
            Ok(())
        };
        watch_loop(
            &store,
            &transport,
            1000,
            100,
            None,
            Duration::from_millis(5),
            shutdown,
        )
        .await
        .unwrap();
        assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
        assert_eq!(store.outbox_len().unwrap(), 0);
    }

    struct PendingSend {
        calls: AtomicUsize,
    }
    impl Transport for PendingSend {
        async fn send(&self, _body: Vec<u8>) -> Result<(), ExportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn watcher_shutdown_during_send_preserves_pending_rows() {
        let (_directory, store) = fixture();
        let transport = PendingSend {
            calls: AtomicUsize::new(0),
        };
        let shutdown = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok(())
        };
        assert_eq!(
            watch_loop(
                &store,
                &transport,
                1000,
                100,
                None,
                Duration::from_millis(5),
                shutdown
            )
            .await
            .unwrap(),
            0
        );
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.outbox_len().unwrap(), 1);
        assert_eq!(store.quarantine_len().unwrap(), 0);
    }

    #[tokio::test]
    async fn watch_stops_on_authentication_failure_and_keeps_rows() {
        let (_directory, store) = fixture();
        let transport = recorder(true, false);
        assert!(matches!(
            watch_loop(
                &store,
                &transport,
                1000,
                100,
                None,
                Duration::from_millis(5),
                std::future::pending()
            )
            .await,
            Err(ExportError::Authentication(401))
        ));
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.outbox_len().unwrap(), 1);
        assert_eq!(store.quarantine_len().unwrap(), 0);
    }
}
