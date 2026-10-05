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
        let client = client_builder(collector_url, timeout_secs)
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
            .map_err(transport_failure)?;
        classify(resp.status())
    }
}

/// Prevent redirect replay and proxy routing from escaping a local collector.
pub(crate) fn client_builder(collector_url: &str, timeout_secs: u64) -> reqwest::ClientBuilder {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .redirect(reqwest::redirect::Policy::none());
    if is_loopback(collector_url) {
        builder = builder.no_proxy();
    }
    builder
}

fn is_loopback(collector_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(collector_url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    host == "localhost"
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Return fixed categories instead of reqwest diagnostics, which can include
/// a request URL or user-controlled value. Response bodies are never read.
pub(crate) fn transport_failure(error: reqwest::Error) -> ExportError {
    let category = if error.is_timeout() {
        "collector request timed out"
    } else if error.is_connect() {
        "collector connection failed"
    } else {
        "collector transport failed"
    };
    ExportError::Unavailable(category.into())
}

/// Separate payload rejection, transient failures and credential failures.
/// Authentication failures preserve the outbox but must not retry the same token.
pub(crate) fn classify(status: reqwest::StatusCode) -> Result<(), ExportError> {
    use reqwest::StatusCode;
    if status.is_success() {
        Ok(())
    } else if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        Err(ExportError::Authentication(status.as_u16()))
    } else if matches!(
        status,
        StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
    ) {
        // Recoverable once the timeout/throttle passes - keep the spans queued.
        Err(ExportError::Unavailable(format!(
            "collector returned {status} (timeout/throttle; retrying)"
        )))
    } else if matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::PAYLOAD_TOO_LARGE | StatusCode::UNPROCESSABLE_ENTITY
    ) {
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
    fn throttle_and_timeout_are_retryable_not_poison() {
        for s in [StatusCode::REQUEST_TIMEOUT, StatusCode::TOO_MANY_REQUESTS] {
            let e = classify(s).unwrap_err();
            assert!(e.retryable(), "{s} must be retryable");
            assert!(matches!(e, ExportError::Unavailable(_)));
        }
    }

    #[tokio::test]
    async fn redirect_cannot_forward_trace_body_to_another_origin() {
        use crate::{Http2Transport, Transport};
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use std::time::{Duration, Instant};

        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let destination = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin_url = format!("http://{}", origin.local_addr().unwrap());
        let redirect_url = format!("http://{}/v1/traces", destination.local_addr().unwrap());
        destination.set_nonblocking(true).unwrap();
        let received = Arc::new(AtomicUsize::new(0));
        let observed = received.clone();
        let redirect_server = std::thread::spawn(move || {
            let (mut stream, _) = origin.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut buffer = [0; 8192];
            let _ = stream.read(&mut buffer).unwrap();
            write!(stream, "HTTP/1.1 307 Temporary Redirect\r\nLocation: {redirect_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let destination_server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(300);
            while Instant::now() < deadline {
                match destination.accept() {
                    Ok((mut stream, _)) => {
                        observed.fetch_add(1, Ordering::SeqCst);
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut buffer = [0; 8192];
                        let _ = stream.read(&mut buffer);
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("synthetic listener failed: {error}"),
                }
            }
        });
        let transport = Http2Transport::new(&origin_url, "synthetic-auth", 1);
        assert!(transport
            .send(b"synthetic trace payload".to_vec())
            .await
            .is_err());
        redirect_server.join().unwrap();
        destination_server.join().unwrap();
        assert_eq!(received.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn transport_error_diagnostics_do_not_include_request_url_or_values() {
        let error = reqwest::Client::new()
            .get("invalid url")
            .build()
            .unwrap_err()
            .with_url(
                "https://synthetic.invalid/?token=synthetic-sensitive-value"
                    .parse()
                    .unwrap(),
            );
        let message = super::transport_failure(error).to_string();
        assert_eq!(message, "collector transport failed");
        assert!(!message.contains("synthetic-sensitive-value"));
        assert!(!message.contains("synthetic.invalid"));
    }

    #[test]
    fn authentication_stops_same_token_retries() {
        for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
            let error = classify(status).unwrap_err();
            assert!(matches!(error, ExportError::Authentication(code) if code == status.as_u16()));
            assert!(!error.retryable());
        }
    }

    #[test]
    fn endpoint_configuration_errors_keep_valid_payloads_queued() {
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::METHOD_NOT_ALLOWED,
            StatusCode::CONFLICT,
        ] {
            let error = classify(status).unwrap_err();
            assert!(matches!(error, ExportError::Unavailable(_)));
            assert!(error.retryable());
        }
    }

    #[test]
    fn loopback_detection_does_not_trust_hostname_prefixes() {
        for url in [
            "http://127.0.0.1:8787",
            "http://127.0.0.2:8787",
            "http://[::1]:8787",
            "http://localhost:8787",
        ] {
            assert!(super::is_loopback(url));
        }
        for url in [
            "https://127.0.0.1.example.invalid",
            "https://localhost.example.invalid",
            "https://collector.example.invalid",
            "invalid URL",
        ] {
            assert!(!super::is_loopback(url));
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
