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
    pub fn new(collector_url: &str, token: impl Into<String>, timeout_secs: u64) -> Self {
        let endpoint = format!("{}/v1/traces", collector_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()
            .expect("reqwest client builds with default rustls config");
        Self {
            endpoint,
            token: token.into(),
            client,
        }
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
            .map_err(|e| ExportError::Unavailable(e.to_string()))?;
        classify(resp.status())
    }
}

/// Map an HTTP status to a delivery outcome. Success is 2xx. Among 4xx we draw
/// the line between *poison* (the payload is genuinely unprocessable and will
/// never succeed - quarantine it) and *transient* (the request was fine but the
/// caller's auth/rate state was wrong - retry once it's fixed). Treating auth
/// (401/403), timeout (408) and rate-limit (429) as poison would silently
/// dead-letter valid spans on a stale token or a throttle. Other failures
/// (5xx / unexpected) are retryable too.
pub(crate) fn classify(status: reqwest::StatusCode) -> Result<(), ExportError> {
    use reqwest::StatusCode;
    if status.is_success() {
        Ok(())
    } else if matches!(
        status,
        StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
            | StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_MANY_REQUESTS
    ) {
        // Recoverable once auth/throttle is corrected - keep the spans queued.
        Err(ExportError::Unavailable(format!(
            "collector returned {status} (auth/throttle; retrying)"
        )))
    } else if status.is_client_error() {
        // Genuine poison: malformed / too large / unprocessable. Retrying the
        // same bytes can never succeed, so quarantine instead of looping.
        Err(ExportError::Rejected(status.as_u16()))
    } else {
        Err(ExportError::Unavailable(format!(
            "collector returned {status}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::classify;
    use crate::ExportError;
    use reqwest::StatusCode;

    #[test]
    fn success_is_ok() {
        assert!(classify(StatusCode::OK).is_ok());
        assert!(classify(StatusCode::ACCEPTED).is_ok());
    }

    #[test]
    fn auth_and_throttle_are_retryable_not_poison() {
        for s in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            let e = classify(s).unwrap_err();
            assert!(e.retryable(), "{s} must be retryable");
            assert!(matches!(e, ExportError::Unavailable(_)));
        }
    }

    #[test]
    fn genuine_bad_request_is_poison() {
        for s in [
            StatusCode::BAD_REQUEST,
            StatusCode::PAYLOAD_TOO_LARGE,
            StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            let e = classify(s).unwrap_err();
            assert!(!e.retryable(), "{s} must be poison");
            assert!(matches!(e, ExportError::Rejected(_)));
        }
    }

    #[test]
    fn server_errors_are_retryable() {
        for s in [
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(classify(s).unwrap_err().retryable(), "{s} retryable");
        }
    }
}
