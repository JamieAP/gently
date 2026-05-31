//! HTTP/2 OTLP transport.
//!
//! Posts OTLP/JSON to the collector's `/v1/traces` endpoint with a bearer token.
//! The client negotiates HTTP/2 over TLS; the wire-version choice is deliberately
//! left to ALPN rather than fought (HTTP/3 would slot in as a separate
//! [`Transport`](crate::Transport) impl behind the same trait).

use crate::{ExportError, Transport};

/// An OTLP/JSON-over-HTTPS transport.
pub struct Http2Transport {
    endpoint: String,
    token: String,
    client: reqwest::Client,
}

impl Http2Transport {
    /// Build a transport targeting `collector_url` (the base URL; `/v1/traces`
    /// is appended) authenticated with `token`.
    pub fn new(collector_url: &str, token: impl Into<String>) -> Self {
        let endpoint = format!("{}/v1/traces", collector_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client builds with default rustls config");
        Self { endpoint, token: token.into(), client }
    }
}

impl Transport for Http2Transport {
    async fn send(&self, body: Vec<u8>) -> Result<(), ExportError> {
        let resp = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| ExportError::Transport(e.to_string()))?;

        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(ExportError::Transport(format!("collector returned {status}")))
        }
    }
}
