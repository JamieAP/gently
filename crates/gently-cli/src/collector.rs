//! Bounded tenant-scoped transport. It never holds raw decryption keys.
use anyhow::Result;
use gently_export::ExportError;
use gently_raw::RawObject;
use serde::de::DeserializeOwned;

pub struct CollectorClient {
    base: String,
    token: String,
    tenant: String,
    client: reqwest::Client,
}
impl CollectorClient {
    pub fn new(base: &str, token: &str, tenant: &str, timeout: u64) -> Result<Self> {
        let url =
            reqwest::Url::parse(base).map_err(|_| anyhow::anyhow!("invalid collector URL"))?;
        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout))
            .redirect(reqwest::redirect::Policy::none());
        if url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        }) {
            builder = builder.no_proxy();
        }
        Ok(Self {
            base: base.trim_end_matches('/').into(),
            token: token.into(),
            tenant: tenant.into(),
            client: builder
                .build()
                .map_err(|_| anyhow::anyhow!("cannot construct collector client"))?,
        })
    }
    pub async fn query<T: DeserializeOwned>(&self, params: &[(&str, String)]) -> Result<T> {
        let response = self
            .client
            .get(format!("{}/v1/query", self.base))
            .query(&[("tenant_id", &self.tenant)])
            .query(params)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("collector query transport failed"))?;
        anyhow::ensure!(
            response.status().is_success(),
            "collector query returned HTTP {}",
            response.status().as_u16()
        );
        let bytes = bounded_response(response, 8 * 1024 * 1024).await?;
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid collector query response"))
    }
    pub async fn fetch_raw(&self, reference: &str) -> Result<Option<RawObject>> {
        anyhow::ensure!(
            reference.len() == 32
                && reference
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid raw reference"
        );
        let response = self
            .client
            .get(format!("{}/v1/raw-values/{reference}", self.base))
            .query(&[("tenant_id", &self.tenant)])
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("ciphertext download transport failed"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        anyhow::ensure!(
            response.status().is_success(),
            "ciphertext download returned HTTP {}",
            response.status().as_u16()
        );
        let bytes = bounded_response(response, 1024 * 1024).await?;
        let object: RawObject = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid ciphertext download response"))?;
        anyhow::ensure!(
            object.context.tenant_id == self.tenant && object.context.raw_ref == reference,
            "ciphertext response namespace mismatch"
        );
        object.validate()?;
        Ok(Some(object))
    }
    pub async fn upload_raw(&self, object: &RawObject) -> Result<(), ExportError> {
        if object.context.tenant_id != self.tenant || object.validate().is_err() {
            return Err(ExportError::Rejected(422));
        }
        let response = self
            .client
            .post(format!("{}/v1/raw-values", self.base))
            .query(&[("tenant_id", &self.tenant)])
            .bearer_auth(&self.token)
            .json(object)
            .send()
            .await
            .map_err(|_| ExportError::Unavailable("ciphertext upload transport failed".into()))?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else if matches!(status.as_u16(), 401 | 403) {
            Err(ExportError::Authentication(status.as_u16()))
        } else if matches!(status.as_u16(), 400 | 409 | 413 | 422) {
            Err(ExportError::Rejected(status.as_u16()))
        } else {
            Err(ExportError::Unavailable(format!(
                "ciphertext upload returned HTTP {}",
                status.as_u16()
            )))
        }
    }
}

async fn bounded_response(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    anyhow::ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= limit as u64),
        "collector response exceeds limit"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("collector response transport failed"))?
    {
        anyhow::ensure!(
            chunk.len() <= limit.saturating_sub(bytes.len()),
            "collector response exceeds limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    pub(crate) fn server(
        status: &str,
        response: &str,
    ) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let status = status.to_string();
        let response = response.to_string();
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap();
                bytes.extend_from_slice(&buf[..n]);
                if n == 0 {
                    break;
                }
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            )
            .unwrap();
            String::from_utf8(bytes).unwrap()
        });
        (base, task)
    }
    #[tokio::test]
    async fn metadata_query_is_authenticated_and_tenant_scoped() {
        let (base, task) = server("200 OK", "[]");
        let client = CollectorClient::new(&base, "synthetic-token", "tenant-a", 2).unwrap();
        let rows: Vec<serde_json::Value> = client.query(&[("op", "traces".into())]).await.unwrap();
        assert!(rows.is_empty());
        let request = task.join().unwrap();
        assert!(request.starts_with("GET /v1/query?tenant_id=tenant-a&op=traces "));
        assert!(request
            .to_lowercase()
            .contains("authorization: bearer synthetic-token"));
    }
    #[tokio::test]
    async fn missing_ciphertext_remains_an_opaque_reference() {
        let (base, task) = server("404 Not Found", "");
        let client = CollectorClient::new(&base, "synthetic-token", "tenant-a", 2).unwrap();
        assert!(client.fetch_raw(&"a".repeat(32)).await.unwrap().is_none());
        let request = task.join().unwrap();
        assert!(request.starts_with(&format!(
            "GET /v1/raw-values/{}?tenant_id=tenant-a ",
            "a".repeat(32)
        )));
    }
    #[tokio::test]
    async fn authentication_failure_does_not_read_sensitive_response_body() {
        let (base, task) = server("403 Forbidden", "synthetic-secret-never-forward");
        let client = CollectorClient::new(&base, "synthetic-token", "tenant-a", 2).unwrap();
        let error = client.fetch_raw(&"a".repeat(32)).await.err().unwrap();
        assert!(!error.to_string().contains("synthetic-secret"));
        task.join().unwrap();
    }
}
