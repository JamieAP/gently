//! Prefer-QUIC transport: try HTTP/3 first, fall back to HTTP/2.
//!
//! A configured preferred transport is attempted first. If it is absent, the
//! fallback is used directly. Failures that lead to a fallback are logged.
//! Constructing the preferred transport and the collector's protocol support
//! determine whether HTTP/3 can be attempted; this wrapper does not establish
//! which protocol a particular deployment successfully uses.

use crate::{ExportError, Transport};

/// Sends over `preferred` (QUIC) when present, otherwise / on failure over
/// `fallback` (HTTP/2). Generic over both transports so the fallback logic is
/// unit-testable with mocks.
pub struct PreferQuic<P: Transport, F: Transport> {
    preferred: Option<P>,
    fallback: F,
}

impl<P: Transport, F: Transport> PreferQuic<P, F> {
    /// `preferred` is `None` when no QUIC transport could be constructed.
    pub fn new(preferred: Option<P>, fallback: F) -> Self {
        Self { preferred, fallback }
    }
}

impl<P: Transport + Sync, F: Transport + Sync> Transport for PreferQuic<P, F> {
    async fn send(&self, body: Vec<u8>) -> Result<(), ExportError> {
        if let Some(preferred) = &self.preferred {
            match preferred.send(body.clone()).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(error = %e, "QUIC send failed; falling back to HTTP/2");
                }
            }
        }
        self.fallback.send(body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Recorder {
        calls: AtomicUsize,
        ok: bool,
    }
    impl Recorder {
        fn new(ok: bool) -> Self {
            Self { calls: AtomicUsize::new(0), ok }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }
    impl Transport for Recorder {
        async fn send(&self, _body: Vec<u8>) -> Result<(), ExportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.ok {
                Ok(())
            } else {
                Err(ExportError::Transport("nope".into()))
            }
        }
    }

    #[tokio::test]
    async fn prefers_quic_when_it_succeeds() {
        let t = PreferQuic::new(Some(Recorder::new(true)), Recorder::new(true));
        t.send(vec![1, 2, 3]).await.unwrap();
        assert_eq!(t.preferred.as_ref().unwrap().calls(), 1);
        assert_eq!(t.fallback.calls(), 0, "fallback untouched when QUIC works");
    }

    #[tokio::test]
    async fn falls_back_to_http2_when_quic_fails() {
        let t = PreferQuic::new(Some(Recorder::new(false)), Recorder::new(true));
        t.send(vec![1, 2, 3]).await.unwrap();
        assert_eq!(t.preferred.as_ref().unwrap().calls(), 1);
        assert_eq!(t.fallback.calls(), 1, "fallback used after QUIC failure");
    }

    #[tokio::test]
    async fn uses_http2_directly_when_no_quic() {
        let t: PreferQuic<Recorder, Recorder> = PreferQuic::new(None, Recorder::new(true));
        t.send(vec![1, 2, 3]).await.unwrap();
        assert_eq!(t.fallback.calls(), 1);
    }
}
