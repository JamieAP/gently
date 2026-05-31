//! Read-side HTTP client for the collector's `/v1/query` surface.
//!
//! Shared by the `gently query` subcommands and the MCP server, so both speak to
//! the collector through one typed interface.

use crate::config::Config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A trace as summarized by the collector's `op=traces`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceSummary {
    pub trace_id: String,
    pub session_id: Option<String>,
    pub harness: Option<String>,
    pub start: Option<String>,
    pub span_count: i64,
    pub error_count: Option<i64>,
}

/// One span row as returned by `op=trace` / `op=spans`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpanRow {
    pub span_id: String,
    pub trace_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: i64,
    pub start_unix_nano: String,
    pub end_unix_nano: Option<String>,
    pub status: i64,
    pub session_id: Option<String>,
    pub harness: Option<String>,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
}

/// Per-tool rollup from `op=stats`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolStat {
    pub tool_name: Option<String>,
    pub span_count: i64,
    pub error_count: Option<i64>,
    pub avg_duration_ms: Option<f64>,
}

/// Filters for `op=spans`.
#[derive(Clone, Debug, Default)]
pub struct SpanFilters {
    pub trace_id: Option<String>,
    pub tool_name: Option<String>,
    pub status: Option<String>,
    pub since: Option<String>,
    pub limit: Option<u32>,
}

/// HTTP client for the collector query API.
pub struct QueryClient {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl QueryClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        cfg.require_collector()?;
        Ok(Self {
            base: format!("{}/v1/query", cfg.collector_url.trim_end_matches('/')),
            token: cfg.token.clone(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(cfg.query_timeout_secs))
                .build()
                .expect("reqwest client builds"),
        })
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, params: &[(&str, String)]) -> Result<T> {
        let resp = self
            .client
            .get(&self.base)
            .query(params)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("collector request failed")?;
        anyhow::ensure!(
            resp.status().is_success(),
            "collector returned {}",
            resp.status()
        );
        resp.json::<T>()
            .await
            .context("decoding collector response")
    }

    pub async fn traces(
        &self,
        limit: Option<u32>,
        harness: Option<&str>,
    ) -> Result<Vec<TraceSummary>> {
        let mut p = vec![("op", "traces".to_string())];
        if let Some(l) = limit {
            p.push(("limit", l.to_string()));
        }
        if let Some(h) = harness {
            p.push(("harness", h.to_string()));
        }
        self.get(&p).await
    }

    pub async fn trace(&self, trace_id: &str) -> Result<Vec<SpanRow>> {
        self.get(&[("op", "trace".into()), ("trace_id", trace_id.to_string())])
            .await
    }

    pub async fn spans(&self, f: &SpanFilters) -> Result<Vec<SpanRow>> {
        let mut p = vec![("op", "spans".to_string())];
        if let Some(v) = &f.trace_id {
            p.push(("trace_id", v.clone()));
        }
        if let Some(v) = &f.tool_name {
            p.push(("tool_name", v.clone()));
        }
        if let Some(v) = &f.status {
            p.push(("status", v.clone()));
        }
        if let Some(v) = &f.since {
            p.push(("since", v.clone()));
        }
        if let Some(v) = f.limit {
            p.push(("limit", v.to_string()));
        }
        self.get(&p).await
    }

    pub async fn stats(&self) -> Result<Vec<ToolStat>> {
        self.get(&[("op", "stats".into())]).await
    }
}
