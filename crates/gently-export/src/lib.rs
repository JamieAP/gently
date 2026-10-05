//! Draining the outbox to the collector.
//!
//! The exporter sends queued OTLP envelopes to the collector, including any
//! provisional spans. Failed retryable sends leave rows queued for a later drain.
//! A drain trims oldest rows above the configured cap before attempting delivery,
//! so an outage can cause data loss. Queueing and delivery do not prove capture
//! completeness. Repeated span IDs replace prior values in the Worker.

mod http2;
mod prefer;
mod quic;

pub use http2::Http2Transport;
pub use prefer::PreferQuic;
pub use quic::QuicTransport;

use gently_core::OtlpRequest;
use gently_store::Store;

// Bound aggregation memory and wire batches. An individual larger envelope is
// still sent alone so the collector decides whether it is acceptable.
const MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;

struct PendingEnvelope {
    id: i64,
    request: OtlpRequest,
    encoded_len: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// Collector unreachable, timed out, returned 5xx, or throttled the request.
    /// Rows stay queued and retrying may succeed without changing credentials.
    #[error("{0}")]
    Unavailable(String),
    /// Valid payload, rejected credentials. Keep rows queued, but stop this run
    /// because another request with the same token cannot fix authentication.
    #[error("collector authentication failed (HTTP {0}); spans remain queued; relaunch exporter with the collector token")]
    Authentication(u16),
    /// Poison: collector reached and rejected the request as genuinely
    /// unprocessable (e.g. 400/413/422) - the same bytes will never succeed, so
    /// the offending envelope is quarantined rather than retried forever.
    /// Its full bytes are retained, including any otherwise-valid sibling spans.
    #[error("collector rejected request (HTTP {0})")]
    Rejected(u16),
    #[error("store: {0}")]
    Store(#[from] gently_store::StoreError),
}

impl ExportError {
    /// Whether an immediate retry with the same credentials could succeed.
    pub fn retryable(&self) -> bool {
        !matches!(
            self,
            ExportError::Rejected(_) | ExportError::Authentication(_)
        )
    }
}

/// A pluggable wire transport for OTLP batches. The HTTP/2 implementation ships
/// today; an HTTP/3 implementation can be added behind this trait without
/// touching the drain loop.
pub trait Transport {
    /// Send one serialized OTLP/JSON body. Returns `Ok` only on a 2xx response.
    fn send(
        &self,
        body: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<(), ExportError>> + Send;
}

/// Drain the outbox into `transport` until it is empty or a batch fails.
///
/// Returns the number of spans successfully delivered. Trims the outbox to
/// `cap` first, logging any drops. Queue length can exceed the cap between
/// drains. Coalesces up to `batch_size` envelopes per wire request, with a
/// 4 MiB aggregation limit. A larger individual envelope is sent alone.
pub async fn drain<T: Transport>(
    store: &Store,
    transport: &T,
    cap: usize,
    batch_size: usize,
) -> Result<usize, ExportError> {
    let dropped = store.outbox_trim(cap)?;
    if dropped > 0 {
        tracing::warn!(dropped, "outbox over capacity; dropped oldest envelopes");
    }

    let mut delivered = 0usize;
    loop {
        let stored = store.outbox_take_batch(batch_size)?;
        if stored.is_empty() {
            break;
        }
        let mut batch = Vec::with_capacity(stored.len());
        for (id, json) in stored {
            match serde_json::from_str::<OtlpRequest>(&json) {
                Ok(request) => batch.push(PendingEnvelope {
                    id,
                    request,
                    encoded_len: json.len(),
                }),
                Err(_) => {
                    // Keep malformed bytes for inspection, with a fixed reason
                    // that cannot echo the payload or a credential it contains.
                    store.outbox_quarantine(&[id], "invalid queued OTLP envelope JSON")?;
                    tracing::warn!(id, "invalid queued OTLP envelope; quarantined");
                }
            }
        }
        // Resolve envelope batches by bisection: a 2xx delivers a slice; a
        // payload rejection on one envelope quarantines it; a rejection on a larger slice
        // splits it to isolate the rejected envelope. Its full contents are
        // retained in quarantine; there is no per-span split within a row. A
        // retryable error (collector down / 5xx / timeout) aborts the whole run
        // - the rows stay queued and the next run (or the cmd_export backoff
        // loop) retries them.
        let mut stack: Vec<(usize, usize)> = vec![(0, batch.len())];
        while let Some((lo, hi)) = stack.pop() {
            if lo >= hi {
                continue;
            }
            let slice = &batch[lo..hi];
            if slice.len() > 1
                && slice.iter().map(|row| row.encoded_len).sum::<usize>() > MAX_BATCH_BYTES
            {
                let mid = lo + (hi - lo) / 2;
                stack.push((lo, mid));
                stack.push((mid, hi));
                continue;
            }
            let (body, span_count) = coalesce(slice);
            if slice.len() > 1 && body.len() > MAX_BATCH_BYTES {
                let mid = lo + (hi - lo) / 2;
                stack.push((lo, mid));
                stack.push((mid, hi));
                continue;
            }
            match transport.send(body).await {
                Ok(()) => {
                    let ids: Vec<i64> = slice.iter().map(|row| row.id).collect();
                    store.outbox_delete(&ids)?;
                    delivered += span_count;
                }
                Err(e) if !matches!(e, ExportError::Rejected(_)) => {
                    tracing::warn!(error = %e, pending = slice.len(), "export paused; rows remain queued");
                    return Err(e);
                }
                Err(e) if hi - lo == 1 => {
                    let id = slice[0].id;
                    tracing::error!(error = %e, id, "collector rejected envelope; quarantining (poison)");
                    store.outbox_quarantine(&[id], &e.to_string())?;
                }
                Err(_rejected) => {
                    let mid = lo + (hi - lo) / 2;
                    stack.push((lo, mid));
                    stack.push((mid, hi));
                }
            }
        }
    }
    Ok(delivered)
}

/// Merge parsed envelopes and count the actual spans in the serialized request.
fn coalesce(batch: &[PendingEnvelope]) -> (Vec<u8>, usize) {
    let merged = OtlpRequest::merge(batch.iter().map(|row| row.request.clone()).collect());
    let count = merged.span_count();
    (
        serde_json::to_vec(&merged).expect("OTLP request serializes"),
        count,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gently_core::{Resource, Span, SpanId, SpanKind, Status, TraceId};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("state.db")).unwrap();
        (dir, s)
    }

    fn enqueue_span(s: &Store, key: &str) {
        enqueue_envelope(s, &[key]);
    }

    fn enqueue_envelope(s: &Store, keys: &[&str]) {
        let spans = keys
            .iter()
            .map(|key| Span {
                trace_id: TraceId::from_session("s"),
                span_id: SpanId::derive("s", key),
                parent_span_id: None,
                name: (*key).into(),
                kind: SpanKind::Internal,
                start_unix_nano: 1,
                end_unix_nano: 2,
                status: Status::Ok,
                attributes: vec![],
            })
            .collect();
        let req = OtlpRequest::single(&Resource::new("s", "claude-code", "/w"), spans);
        s.outbox_enqueue(&serde_json::to_string(&req).unwrap())
            .unwrap();
    }

    /// Fails the first `fail_n` sends, then succeeds; counts spans received.
    struct FlakyTransport {
        calls: AtomicUsize,
        fail_n: usize,
        spans_received: AtomicUsize,
    }

    impl Transport for FlakyTransport {
        async fn send(&self, body: Vec<u8>) -> Result<(), ExportError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_n {
                return Err(ExportError::Unavailable("flaky".into()));
            }
            let req: OtlpRequest = serde_json::from_slice(&body).unwrap();
            self.spans_received
                .fetch_add(req.span_count(), Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn mixed_single_and_multispan_envelopes_count_delivered_spans() {
        let (_d, s) = store();
        enqueue_span(&s, "old-single");
        enqueue_envelope(&s, &["session", "turn", "tool", "terminal"]);
        let t = FlakyTransport {
            calls: AtomicUsize::new(0),
            fail_n: 0,
            spans_received: AtomicUsize::new(0),
        };
        assert_eq!(drain(&s, &t, 10_000, 512).await.unwrap(), 5);
        assert_eq!(t.spans_received.load(Ordering::SeqCst), 5);
        assert_eq!(t.calls.load(Ordering::SeqCst), 1);
        assert_eq!(s.outbox_len().unwrap(), 0);
    }

    #[tokio::test]
    async fn malformed_envelopes_are_quarantined_without_losing_good_rows() {
        let (_directory, store) = store();
        store
            .outbox_enqueue("malformed synthetic-sensitive-payload")
            .unwrap();
        enqueue_span(&store, "good-single");
        enqueue_envelope(&store, &["good-turn", "good-tool"]);
        let transport = FlakyTransport {
            calls: AtomicUsize::new(0),
            fail_n: 0,
            spans_received: AtomicUsize::new(0),
        };
        assert_eq!(drain(&store, &transport, 10_000, 512).await.unwrap(), 3);
        assert_eq!(store.outbox_len().unwrap(), 0);
        assert_eq!(store.quarantine_len().unwrap(), 1);
        assert_eq!(transport.spans_received.load(Ordering::SeqCst), 3);
    }

    struct ByteRecorder {
        calls: AtomicUsize,
        largest_body: AtomicUsize,
    }
    impl Transport for ByteRecorder {
        async fn send(&self, body: Vec<u8>) -> Result<(), ExportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.largest_body.fetch_max(body.len(), Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn aggregated_payload_is_split_before_exceeding_wire_byte_limit() {
        let (_directory, store) = store();
        let large_name = "synthetic".repeat(280_000);
        enqueue_span(&store, &large_name);
        enqueue_span(&store, &large_name);
        let transport = ByteRecorder {
            calls: AtomicUsize::new(0),
            largest_body: AtomicUsize::new(0),
        };
        assert_eq!(drain(&store, &transport, 10_000, 512).await.unwrap(), 2);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
        assert!(transport.largest_body.load(Ordering::SeqCst) <= 4 * 1024 * 1024);
        assert_eq!(store.outbox_len().unwrap(), 0);
    }

    struct AuthenticationRejector {
        calls: AtomicUsize,
    }
    impl Transport for AuthenticationRejector {
        async fn send(&self, _body: Vec<u8>) -> Result<(), ExportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(ExportError::Authentication(401))
        }
    }

    #[tokio::test]
    async fn auth_failure_preserves_entire_batch_without_bisection_or_quarantine() {
        let (_d, s) = store();
        enqueue_span(&s, "old-single");
        enqueue_envelope(&s, &["session", "turn", "tool"]);
        let t = AuthenticationRejector {
            calls: AtomicUsize::new(0),
        };
        assert!(matches!(
            drain(&s, &t, 10_000, 512).await,
            Err(ExportError::Authentication(401))
        ));
        assert_eq!(t.calls.load(Ordering::SeqCst), 1);
        assert_eq!(s.outbox_len().unwrap(), 2);
        assert_eq!(s.quarantine_len().unwrap(), 0);
    }

    /// Rejects (400) any batch containing a span named "poison".
    struct PoisonRejector;
    impl Transport for PoisonRejector {
        async fn send(&self, body: Vec<u8>) -> Result<(), ExportError> {
            let req: OtlpRequest = serde_json::from_slice(&body).unwrap();
            let has_poison = req
                .resource_spans
                .iter()
                .flat_map(|rs| rs.scope_spans.iter())
                .flat_map(|ss| ss.spans.iter())
                .any(|sp| sp.name == "poison");
            if has_poison {
                Err(ExportError::Rejected(400))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn poison_span_is_quarantined_others_delivered() {
        let (_d, s) = store();
        enqueue_span(&s, "good1");
        enqueue_span(&s, "poison");
        enqueue_span(&s, "good2");

        // One batch contains the poison span -> drain bisects, quarantines it,
        // and delivers the two good spans, returning Ok (not an error).
        let delivered = drain(&s, &PoisonRejector, 10_000, 512).await.unwrap();
        assert_eq!(delivered, 2);
        assert_eq!(s.outbox_len().unwrap(), 0, "queue drained");
        assert_eq!(s.quarantine_len().unwrap(), 1, "poison span quarantined");
    }

    #[tokio::test]
    async fn rejected_multispan_envelope_is_retained_as_one_quarantine_item() {
        let (_directory, store) = store();
        enqueue_envelope(&store, &["good1", "poison", "good2"]);
        assert_eq!(
            drain(&store, &PoisonRejector, 10_000, 512).await.unwrap(),
            0
        );
        assert_eq!(store.outbox_len().unwrap(), 0);
        assert_eq!(store.quarantine_len().unwrap(), 1);
    }

    #[tokio::test]
    async fn failed_send_keeps_rows_then_succeeds_on_retry() {
        let (_d, s) = store();
        enqueue_span(&s, "turn:1");
        enqueue_span(&s, "tool:tu_1");

        let t = FlakyTransport {
            calls: AtomicUsize::new(0),
            fail_n: 2,
            spans_received: AtomicUsize::new(0),
        };

        // first two drains fail -> rows survive for a later retry
        assert!(drain(&s, &t, 10_000, 512).await.is_err());
        assert_eq!(s.outbox_len().unwrap(), 2);
        assert!(drain(&s, &t, 10_000, 512).await.is_err());
        assert_eq!(s.outbox_len().unwrap(), 2);

        // third drain succeeds -> rows gone, 2 spans delivered in one batch
        let delivered = drain(&s, &t, 10_000, 512).await.unwrap();
        assert_eq!(delivered, 2);
        assert_eq!(s.outbox_len().unwrap(), 0);
        assert_eq!(t.spans_received.load(Ordering::SeqCst), 2);
    }
}
