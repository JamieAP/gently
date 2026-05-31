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

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// Collector unreachable, timed out, or returned 5xx - retryable; the
    /// collector's fault, not the payload's. Rows stay queued for the next run.
    #[error("{0}")]
    Unavailable(String),
    /// Collector reached but rejected the request (4xx) - the payload is bad and
    /// will never succeed, so the offending span is quarantined, not retried.
    #[error("collector rejected request (HTTP {0})")]
    Rejected(u16),
    #[error("store: {0}")]
    Store(#[from] gently_store::StoreError),
}

impl ExportError {
    /// Whether retrying could succeed. A 4xx rejection cannot; everything else
    /// (connection failure, timeout, 5xx, a transient store error) might.
    pub fn retryable(&self) -> bool {
        !matches!(self, ExportError::Rejected(_))
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
/// drains. Coalesces up to `batch_size` rows per wire request.
pub async fn drain<T: Transport>(
    store: &Store,
    transport: &T,
    cap: usize,
    batch_size: usize,
) -> Result<usize, ExportError> {
    let dropped = store.outbox_trim(cap)?;
    if dropped > 0 {
        tracing::warn!(dropped, "outbox over capacity; dropped oldest spans");
    }

    let mut delivered = 0usize;
    loop {
        let batch = store.outbox_take_batch(batch_size)?;
        if batch.is_empty() {
            break;
        }
        // Resolve the batch by bisection: a 2xx delivers a slice; a 4xx on a
        // slice of one quarantines that poison span; a 4xx on a larger slice
        // splits it to isolate the culprit without dropping good spans. A
        // retryable error (collector down / 5xx / timeout) aborts the whole run
        // - the rows stay queued and the next run (or the cmd_export backoff
        // loop) retries them.
        let mut stack: Vec<(usize, usize)> = vec![(0, batch.len())];
        while let Some((lo, hi)) = stack.pop() {
            if lo >= hi {
                continue;
            }
            let slice = &batch[lo..hi];
            match transport.send(coalesce(slice)).await {
                Ok(()) => {
                    let ids: Vec<i64> = slice.iter().map(|(id, _)| *id).collect();
                    store.outbox_delete(&ids)?;
                    delivered += slice.len();
                }
                Err(e) if e.retryable() => {
                    tracing::warn!(error = %e, pending = slice.len(), "export unavailable; will retry");
                    return Err(e);
                }
                Err(e) if hi - lo == 1 => {
                    let id = slice[0].0;
                    tracing::error!(error = %e, id, "collector rejected span; quarantining (poison)");
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

/// Parse each stored single-span OTLP request and merge them into one body.
/// Rows that fail to parse are skipped (they cannot block the rest).
fn coalesce(batch: &[(i64, String)]) -> Vec<u8> {
    let reqs: Vec<OtlpRequest> = batch
        .iter()
        .filter_map(|(_, json)| serde_json::from_str(json).ok())
        .collect();
    let merged = OtlpRequest::merge(reqs);
    serde_json::to_vec(&merged).unwrap_or_else(|_| b"{\"resourceSpans\":[]}".to_vec())
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
        let span = Span {
            trace_id: TraceId::from_session("s"),
            span_id: SpanId::derive("s", key),
            parent_span_id: None,
            name: key.into(),
            kind: SpanKind::Internal,
            start_unix_nano: 1,
            end_unix_nano: 2,
            status: Status::Ok,
            attributes: vec![],
        };
        let req = OtlpRequest::single(&Resource::new("s", "claude-code", "/w"), vec![span]);
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
