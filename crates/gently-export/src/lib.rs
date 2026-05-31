//! Draining the outbox to the collector.
//!
//! The exporter sends queued OTLP envelopes to the collector, including any
//! provisional spans. Failed retryable sends leave rows queued for a later drain.
//! A drain trims oldest rows above the configured cap before attempting delivery,
//! so an outage can cause data loss. Queueing and delivery do not prove capture
//! completeness. Repeated span IDs replace prior values in the Worker.
//! Failed sends increment attempt counters in this version.

mod http2;

pub use http2::Http2Transport;

use gently_core::OtlpRequest;
use gently_store::{Store, OUTBOX_CAP};

/// How many outbox rows to coalesce into a single wire request.
const BATCH: usize = 512;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("store: {0}")]
    Store(#[from] gently_store::StoreError),
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
/// [`OUTBOX_CAP`] first, logging any drops. This bounds rows during a drain,
/// not while hooks enqueue without a drain.
pub async fn drain<T: Transport>(store: &Store, transport: &T) -> Result<usize, ExportError> {
    let dropped = store.outbox_trim(OUTBOX_CAP)?;
    if dropped > 0 {
        tracing::warn!(dropped, "outbox over capacity; dropped oldest spans");
    }

    let mut delivered = 0usize;
    loop {
        let batch = store.outbox_take_batch(BATCH)?;
        if batch.is_empty() {
            break;
        }
        let ids: Vec<i64> = batch.iter().map(|(id, _)| *id).collect();
        let body = coalesce(&batch);

        match transport.send(body).await {
            Ok(()) => {
                store.outbox_delete(&ids)?;
                delivered += ids.len();
            }
            Err(e) => {
                store.outbox_bump_attempts(&ids)?;
                tracing::warn!(error = %e, batch = ids.len(), "export batch failed; will retry");
                return Err(e);
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
        s.outbox_enqueue(&serde_json::to_string(&req).unwrap()).unwrap();
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
                return Err(ExportError::Transport("flaky".into()));
            }
            let req: OtlpRequest = serde_json::from_slice(&body).unwrap();
            self.spans_received.fetch_add(req.span_count(), Ordering::SeqCst);
            Ok(())
        }
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

        // first two drains fail -> rows survive, attempts bumped
        assert!(drain(&s, &t).await.is_err());
        assert_eq!(s.outbox_len().unwrap(), 2);
        assert!(drain(&s, &t).await.is_err());
        assert_eq!(s.outbox_len().unwrap(), 2);

        // third drain succeeds -> rows gone, 2 spans delivered in one batch
        let delivered = drain(&s, &t).await.unwrap();
        assert_eq!(delivered, 2);
        assert_eq!(s.outbox_len().unwrap(), 0);
        assert_eq!(t.spans_received.load(Ordering::SeqCst), 2);
    }
}
