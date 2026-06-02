//! Read-side HTTP client for the collector's `/v1/query` surface.
//!
//! Shared by the `gently query` subcommands and the MCP server, so both speak to
//! the collector through one typed interface.

use crate::config::Config;
use crate::local_raw;
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
    /// Raw OTLP resource attributes as a JSON array string, as the collector
    /// stores them (carries `gently.tmux_pane`, `gently.transcript_path`,
    /// `gently.cwd`, …). `None` if the collector omits it. Parse with
    /// [`SpanRow::resource_attr`].
    #[serde(default)]
    pub resource_json: Option<String>,
    /// Raw OTLP span attributes as a JSON array string (`gently.event`,
    /// `gently.tool_name`, digests, …). `None` if omitted.
    #[serde(default)]
    pub attrs_json: Option<String>,
}

impl SpanRow {
    /// Extract one resource attribute's string value from [`Self::resource_json`]
    /// (the OTLP `[{key,value:{stringValue}}]` shape). Returns `None` when the
    /// blob is absent, unparseable, or lacks the key.
    pub fn resource_attr(&self, key: &str) -> Option<String> {
        let blob = self.resource_json.as_deref()?;
        let attrs: serde_json::Value = serde_json::from_str(blob).ok()?;
        attrs.as_array()?.iter().find_map(|kv| {
            (kv.get("key")?.as_str()? == key)
                .then(|| {
                    kv.get("value")?
                        .get("stringValue")?
                        .as_str()
                        .map(String::from)
                })
                .flatten()
        })
    }
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
    pub session_id: Option<String>,
    pub harness: Option<String>,
    pub tool_name: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub kind: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: Option<u32>,
    pub order: Option<String>,
}

/// Filters for `op=traces`.
#[derive(Clone, Debug, Default)]
pub struct TraceFilters {
    pub limit: Option<u32>,
    pub harness: Option<String>,
    pub session_id: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub order: Option<String>,
}

/// HTTP client for the collector query API.
pub struct QueryClient {
    base: String,
    token: String,
    client: reqwest::Client,
    local_raw_store: Option<gently_store::Store>,
}

impl QueryClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        cfg.require_collector()?;
        let local_raw_store = if local_raw::resolve_enabled() {
            cfg.ensure_state_dir()?;
            Some(gently_store::Store::open(&cfg.state_db())?)
        } else {
            None
        };
        Ok(Self {
            base: format!("{}/v1/query", cfg.collector_url.trim_end_matches('/')),
            token: cfg.token.clone(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(cfg.query_timeout_secs))
                .build()
                .expect("reqwest client builds"),
            local_raw_store,
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

    pub async fn traces(&self, f: &TraceFilters) -> Result<Vec<TraceSummary>> {
        let mut p = vec![("op", "traces".to_string())];
        if let Some(l) = f.limit {
            p.push(("limit", l.to_string()));
        }
        if let Some(v) = &f.harness {
            p.push(("harness", v.clone()));
        }
        if let Some(v) = &f.session_id {
            p.push(("session_id", v.clone()));
        }
        if let Some(v) = &f.since {
            p.push(("since", v.clone()));
        }
        if let Some(v) = &f.until {
            p.push(("until", v.clone()));
        }
        if let Some(v) = &f.order {
            p.push(("order", v.clone()));
        }
        self.get(&p).await
    }

    pub async fn trace(&self, trace_id: &str) -> Result<Vec<SpanRow>> {
        let mut rows: Vec<SpanRow> = self
            .get(&[("op", "trace".into()), ("trace_id", trace_id.to_string())])
            .await?;
        self.resolve_local_raw_values(&mut rows)?;
        Ok(rows)
    }

    pub async fn spans(&self, f: &SpanFilters) -> Result<Vec<SpanRow>> {
        let mut p = vec![("op", "spans".to_string())];
        if let Some(v) = &f.trace_id {
            p.push(("trace_id", v.clone()));
        }
        if let Some(v) = &f.session_id {
            p.push(("session_id", v.clone()));
        }
        if let Some(v) = &f.harness {
            p.push(("harness", v.clone()));
        }
        if let Some(v) = &f.tool_name {
            p.push(("tool_name", v.clone()));
        }
        if let Some(v) = &f.name {
            p.push(("name", v.clone()));
        }
        if let Some(v) = &f.status {
            p.push(("status", v.clone()));
        }
        if let Some(v) = &f.kind {
            p.push(("kind", v.clone()));
        }
        if let Some(v) = &f.since {
            p.push(("since", v.clone()));
        }
        if let Some(v) = &f.until {
            p.push(("until", v.clone()));
        }
        if let Some(v) = f.limit {
            p.push(("limit", v.to_string()));
        }
        if let Some(v) = &f.order {
            p.push(("order", v.clone()));
        }
        let mut rows: Vec<SpanRow> = self.get(&p).await?;
        self.resolve_local_raw_values(&mut rows)?;
        Ok(rows)
    }

    pub async fn stats(&self) -> Result<Vec<ToolStat>> {
        self.get(&[("op", "stats".into())]).await
    }

    fn resolve_local_raw_values(&self, rows: &mut [SpanRow]) -> Result<()> {
        if let Some(store) = &self.local_raw_store {
            local_raw::resolve_rows(store, rows)?;
        }
        Ok(())
    }
}
