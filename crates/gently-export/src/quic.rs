//! HTTP/3 (QUIC) transport - in-process, via reqwest's `http3` feature.
//!
//! reqwest already carries our HTTP/2 client, TLS, ALPN and connection pooling,
//! so enabling its `http3` feature gives genuine in-process QUIC (quinn/h3 under
//! the hood) without adding a second, parallel HTTP stack. The client is built
//! with [`http3_prior_knowledge`](reqwest::ClientBuilder::http3_prior_knowledge),
//! so it speaks HTTP/3 only - there is no silent in-client downgrade. Falling
//! back to HTTP/2 is the explicit job of [`PreferQuic`](crate::PreferQuic).
//!
//! IMPORTANT (hot path): constructing this transport builds a QUIC client and is
//! only ever done inside the detached `gently export` process. The `gently hook`
//! hot path never touches this module - no client build, no TLS, no QUIC code.

use crate::{ExportError, Transport};
use std::sync::Once;

/// reqwest's HTTP/3 path uses rustls' no-bundled-provider variant, so a
/// process-default [`CryptoProvider`](rustls::crypto::CryptoProvider) must exist
/// before the QUIC client is built. Install ring once; an `Err` means another
/// provider is already installed, which is equally fine.
static INSTALL_PROVIDER: Once = Once::new();

fn ensure_crypto_provider() {
    INSTALL_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// In-process HTTP/3 transport to the collector's `/v1/traces`.
pub struct QuicTransport {
    endpoint: String,
    token: String,
    client: reqwest::Client,
}

impl QuicTransport {
    /// Build an HTTP/3-only client for `collector_url`. Returns an error if the
    /// client cannot be constructed; the caller then runs HTTP/2 only.
    pub fn new(
        collector_url: &str,
        token: impl Into<String>,
        tenant_id: &str,
        timeout_secs: u64,
    ) -> Result<Self, ExportError> {
        ensure_crypto_provider();
        let endpoint = crate::http2::tenant_endpoint(collector_url, tenant_id)?;
        let client = crate::http2::client_builder(collector_url, timeout_secs)
            .http3_prior_knowledge()
            .build()
            .map_err(|_| ExportError::Unavailable("building HTTP/3 client failed".into()))?;
        Ok(Self {
            endpoint,
            token: token.into(),
            client,
        })
    }
}

impl Transport for QuicTransport {
    async fn send(&self, body: Vec<u8>) -> Result<(), ExportError> {
        let resp = self
            .client
            .post(&self.endpoint)
            .version(reqwest::Version::HTTP_3)
            .bearer_auth(&self.token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(crate::http2::transport_failure)?;
        crate::http2::classify(resp.status())
    }
}
