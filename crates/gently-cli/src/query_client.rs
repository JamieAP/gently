//! Read-side HTTP client for the collector's `/v1/query` surface.
//!
//! Shared by the `gently query` subcommands and the MCP server, so both speak to
//! the collector through one typed interface.

use crate::config::Config;
use crate::local_raw;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use zeroize::Zeroize;

/// A trace as summarized by the collector's `op=traces`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceSummary {
    pub trace_id: String,
    pub session_id: Option<String>,
    pub harness: Option<String>,
    pub start: Option<String>,
    #[serde(default)]
    pub last_activity: Option<String>,
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
    /// OTLP span attributes as a JSON array string (`gently.event`,
    /// `gently.tool_name`, lengths, raw refs, …). `None` if omitted.
    #[serde(default)]
    pub attrs_json: Option<String>,
    /// Collector-derived display bounds from this span and available trace-wide
    /// or direct-child aggregate observations. Non-root records retain existing
    /// non-provisional ends; these bounds do not prove completeness or completion.
    /// `None` from an older collector that predates the derivation; fall back to
    /// the raw start/end. Passed through to MCP `get_trace` consumers.
    #[serde(default)]
    pub effective_start_unix_nano: Option<String>,
    #[serde(default)]
    pub effective_end_unix_nano: Option<String>,
}

impl SpanRow {
    /// Extract one resource attribute's string value from [`Self::resource_json`]
    /// (the OTLP `[{key,value:{stringValue}}]` shape). Returns `None` when the
    /// blob is absent, unparseable, or lacks the key.
    pub fn resource_attr(&self, key: &str) -> Option<String> {
        let blob = self.resource_json.as_deref()?;
        let attrs = crate::json_fidelity::parse(blob).ok()?;
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

#[derive(Deserialize)]
#[serde(untagged)]
enum TraceResponse {
    Page(TracePage),
    Legacy(Vec<SpanRow>),
}
fn is_trace_changed(error: &anyhow::Error) -> bool {
    error.is::<crate::collector::TraceChanged>()
}

#[cfg(unix)]
fn is_old_watcher(error: &anyhow::Error) -> bool {
    error.is::<crate::query_broker::UnsupportedQueryParameter>()
}
#[cfg(not(unix))]
fn is_old_watcher(_: &anyhow::Error) -> bool {
    false
}

#[derive(Deserialize)]
struct TracePage {
    rows: Vec<SpanRow>,
    next_cursor: Option<String>,
    complete: bool,
}
const MAX_TRACE_BYTES: usize = 32 * 1024 * 1024;
const MAX_TRACE_ROWS: usize = 100_000;
/// A paged read restarts from the first page when the trace changes under it.
const TRACE_READ_ATTEMPTS: usize = 3;

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

/// What a page asks the reader to do next.
enum TraceNext {
    Done,
    Continue(String),
}

/// Rows accepted so far in one paged read, with the limits that bound it.
struct TraceAssembly<'a> {
    trace_id: &'a str,
    byte_limit: usize,
    over_limit: QueryFailure,
    rows: Vec<SpanRow>,
    spans: std::collections::BTreeSet<String>,
    cursors: std::collections::BTreeSet<String>,
    bytes: usize,
}
impl<'a> TraceAssembly<'a> {
    fn new(trace_id: &'a str, byte_limit: usize, over_limit: QueryFailure) -> Self {
        Self {
            trace_id,
            byte_limit,
            over_limit,
            rows: Vec::new(),
            spans: Default::default(),
            cursors: Default::default(),
            bytes: 0,
        }
    }

    fn accept(&mut self, page: TracePage) -> Result<TraceNext> {
        let next = match (page.complete, page.next_cursor, page.rows.is_empty()) {
            (true, None, _) => TraceNext::Done,
            (false, Some(_), true) => anyhow::bail!("empty unfinished trace page"),
            (false, Some(cursor), false) => TraceNext::Continue(cursor),
            (true, Some(_), _) | (false, None, _) => {
                anyhow::bail!("inconsistent trace pagination response")
            }
        };
        for row in page.rows {
            self.push(row)?;
        }
        if let TraceNext::Continue(cursor) = &next {
            anyhow::ensure!(
                !cursor.is_empty() && cursor.len() <= 4096 && self.cursors.insert(cursor.clone()),
                "invalid or repeated trace cursor"
            );
        }
        Ok(next)
    }

    fn push(&mut self, row: SpanRow) -> Result<()> {
        anyhow::ensure!(
            row.trace_id == self.trace_id,
            "collector returned a different trace"
        );
        // A repeated span means its position moved during the read.
        if !self.spans.insert(row.span_id.clone()) {
            return Err(crate::collector::TraceChanged.into());
        }
        self.bytes = self.bytes.saturating_add(serde_json::to_vec(&row)?.len());
        anyhow::ensure!(
            self.bytes <= self.byte_limit && self.rows.len() < MAX_TRACE_ROWS,
            self.over_limit
        );
        self.rows.push(row);
        Ok(())
    }
}

/// HTTP client for the collector query API.
pub struct QueryClient {
    collector: crate::collector::CollectorClient,
    tenant_id: String,
    local_raw_store: Option<gently_store::Store>,
    #[cfg(unix)]
    query_socket: Option<PathBuf>,
    #[cfg(unix)]
    broker_base: String,
    #[cfg(unix)]
    timeout: std::time::Duration,
    raw_identity_path: Option<PathBuf>,
    raw_identities: OnceCell<gently_raw::ReaderIdentities>,
}

/// Plaintext lives only for one query, then its temporary field copies are
/// cleared. The persistent store caches encrypted objects exclusively.
struct CachedRawPayload(gently_raw::RawPayload);
impl Drop for CachedRawPayload {
    fn drop(&mut self) {
        for value in self.0.fields.values_mut() {
            value.zeroize();
        }
    }
}

/// Closed safe categories for callers that must not expose error chains. Each
/// names the component that failed; an untagged error is a collector failure.
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum QueryFailure {
    #[error("raw resolution exceeds the 8 MiB budget; narrow your query")]
    RawBudget,
    #[error("Reader identity is unavailable; configure or unlock an enrolled reader.")]
    ReaderUnavailable,
    #[error(
        "Reader could not decrypt raw values; approve the reader prompt within 60 seconds, or use a reader enrolled for these objects."
    )]
    RawDecrypt,
    #[error(
        "Local query watcher failed; restart the unlocked watcher and check collector health."
    )]
    WatcherUnavailable,
    #[error(
        "Local query watcher relayed a collector or query error; check collector availability, authorization and query arguments."
    )]
    WatcherRelayed,
    #[error(
        "trace exceeds the 8 MiB raw-resolution budget; \
         read it without raw values (GENTLY_RESOLVE_RAW_VALUES=0)"
    )]
    TraceRawBudget,
    #[error(
        "trace exceeds local assembly limits (100,000 rows or 32 MiB); \
         use the collector cursor API for bounded pages"
    )]
    TraceTooLarge,
    #[error(
        "trace changed during pagination on {TRACE_READ_ATTEMPTS} attempts; \
         repeat the query once capture for it is idle"
    )]
    TraceUnstable,
}

/// Decryption fails locally: a reader prompt not approved in time, a reader
/// that failed, or an identity that is not a recipient of an older object.
fn open_raw(
    object: &gently_raw::RawObject,
    identities: &gently_raw::ReaderIdentities,
) -> Result<gently_raw::RawPayload> {
    gently_raw::open(object, &object.context, identities).context(QueryFailure::RawDecrypt)
}

const MAX_RAW_QUERY_BYTES: usize = 8 * 1024 * 1024;
fn account_raw_query_bytes(bytes: &mut usize, added: usize) -> Result<()> {
    *bytes = bytes
        .checked_add(added)
        .filter(|total| *total <= MAX_RAW_QUERY_BYTES)
        .context(QueryFailure::RawBudget)?;
    Ok(())
}

fn hydrate_cached_payload(
    tenant_id: &str,
    row: &mut SpanRow,
    payload: &gently_raw::RawPayload,
    bytes: &mut usize,
) -> Result<()> {
    let previous = row.attrs_json.as_deref().map_or(0, str::len);
    local_raw::resolve_payload(tenant_id, row, payload)?;
    let expanded = row.attrs_json.as_deref().map_or(0, str::len);
    if expanded >= previous {
        account_raw_query_bytes(bytes, expanded - previous)?;
    } else {
        *bytes -= previous - expanded;
    }
    Ok(())
}

impl QueryClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        cfg.require_collector_url()?;
        #[cfg(unix)]
        let query_socket = if cfg.token.is_empty() {
            let path = cfg.runtime_dir().join("query.sock");
            crate::query_broker::validate_socket(&path).context(
                "token is not configured; start an unlocked gently export --watch --serve-queries or inherit GENTLY_TOKEN"
            )?;
            Some(path)
        } else {
            None
        };
        #[cfg(not(unix))]
        cfg.require_collector()?;
        if !cfg.token.is_empty() {
            cfg.require_collector()?;
        }
        let local_raw_store = if cfg.resolve_raw_values {
            cfg.ensure_state_dir()?;
            Some(gently_store::Store::open(&cfg.state_db())?)
        } else {
            None
        };
        Ok(Self {
            collector: crate::collector::CollectorClient::new(
                &cfg.collector_url,
                &cfg.token,
                &cfg.tenant_id,
                cfg.query_timeout_secs,
            )?,
            tenant_id: cfg.tenant_id.clone(),
            raw_identity_path: cfg.raw_identity.clone(),
            raw_identities: OnceCell::new(),
            local_raw_store,
            #[cfg(unix)]
            query_socket,
            #[cfg(unix)]
            broker_base: cfg.collector_url.clone(),
            #[cfg(unix)]
            timeout: std::time::Duration::from_secs(cfg.query_timeout_secs.max(1)),
        })
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, params: &[(&str, String)]) -> Result<T> {
        #[cfg(unix)]
        if let Some(path) = &self.query_socket {
            return crate::query_broker::query(
                path,
                &self.broker_base,
                &self.tenant_id,
                params,
                self.timeout,
            )
            .await;
        }
        self.collector.query(params).await
    }

    async fn fetch_raw(&self, reference: &str) -> Result<Option<gently_raw::RawObject>> {
        #[cfg(unix)]
        if let Some(path) = &self.query_socket {
            return crate::query_broker::query(
                path,
                &self.broker_base,
                &self.tenant_id,
                &[("op", "raw".into()), ("raw_ref", reference.into())],
                self.timeout,
            )
            .await;
        }
        self.collector.fetch_raw(reference).await
    }

    fn reader_identities(&self) -> Result<&gently_raw::ReaderIdentities> {
        if self.raw_identities.get().is_none() {
            let path = self
                .raw_identity_path
                .as_deref()
                .context(QueryFailure::ReaderUnavailable)?;
            gently_store::private_fs::harden_existing_file(path)
                .context(QueryFailure::ReaderUnavailable)?;
            let identities =
                gently_raw::load_identities(path).context(QueryFailure::ReaderUnavailable)?;
            let _ = self.raw_identities.set(identities);
        }
        self.raw_identities
            .get()
            .context(QueryFailure::ReaderUnavailable)
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

    /// Raw resolution charges every assembled row against its 8 MiB budget,
    /// so with it enabled the read stops there rather than after every page.
    fn trace_assembly<'a>(&self, trace_id: &'a str) -> TraceAssembly<'a> {
        match self.local_raw_store {
            Some(_) => {
                TraceAssembly::new(trace_id, MAX_RAW_QUERY_BYTES, QueryFailure::TraceRawBudget)
            }
            None => TraceAssembly::new(trace_id, MAX_TRACE_BYTES, QueryFailure::TraceTooLarge),
        }
    }

    async fn fetch_trace_page(&self, trace_id: &str, cursor: Option<&str>) -> Result<TracePage> {
        let mut params = vec![
            ("op", "trace".into()),
            ("trace_id", trace_id.into()),
            ("page", "1".into()),
        ];
        if let Some(cursor) = cursor {
            params.push(("cursor", cursor.into()))
        }
        let response = match (self.get(&params).await, cursor) {
            (Ok(response), _) => response,
            (Err(error), None) if is_old_watcher(&error) => {
                self.fetch_legacy_trace(trace_id).await?
            }
            (Err(error), _) => return Err(error),
        };
        match (response, cursor) {
            (TraceResponse::Page(page), _) => Ok(page),
            (TraceResponse::Legacy(rows), None) => Ok(TracePage {
                rows,
                next_cursor: None,
                complete: true,
            }),
            (TraceResponse::Legacy(_), Some(_)) => {
                anyhow::bail!("legacy trace response after pagination started")
            }
        }
    }

    /// A watcher started before paged queries rejects `page`; ask it for the
    /// complete array instead, which it still serves within its old limits.
    async fn fetch_legacy_trace(&self, trace_id: &str) -> Result<TraceResponse> {
        self.get(&[("op", "trace".into()), ("trace_id", trace_id.into())])
            .await
            .context(
                "the local query watcher predates paged trace queries; \
                 restart gently export --watch --serve-queries after upgrading",
            )
    }

    async fn read_trace(&self, trace_id: &str) -> Result<Vec<SpanRow>> {
        let mut assembly = self.trace_assembly(trace_id);
        let mut cursor = None;
        loop {
            let page = self.fetch_trace_page(trace_id, cursor.as_deref()).await?;
            match assembly.accept(page)? {
                TraceNext::Done => return Ok(assembly.rows),
                TraceNext::Continue(next) => cursor = Some(next),
            }
        }
    }

    pub async fn trace(&self, trace_id: &str) -> Result<Vec<SpanRow>> {
        let mut attempt = 1;
        let mut rows = loop {
            match self.read_trace(trace_id).await {
                Err(error) if is_trace_changed(&error) && attempt < TRACE_READ_ATTEMPTS => {
                    attempt += 1
                }
                Err(error) if is_trace_changed(&error) => {
                    return Err(QueryFailure::TraceUnstable.into())
                }
                result => break result?,
            }
        };
        self.resolve_raw_values(&mut rows).await?;
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
        self.resolve_raw_values(&mut rows).await?;
        Ok(rows)
    }

    pub async fn stats(&self) -> Result<Vec<ToolStat>> {
        self.get(&[("op", "stats".into())]).await
    }

    async fn resolve_raw_values(&self, rows: &mut [SpanRow]) -> Result<()> {
        self.resolve_raw_values_with_reader(rows, |object| {
            open_raw(object, self.reader_identities()?)
        })
        .await
    }

    async fn resolve_raw_values_with_reader<F>(
        &self,
        rows: &mut [SpanRow],
        mut decrypt: F,
    ) -> Result<()>
    where
        F: FnMut(&gently_raw::RawObject) -> Result<gently_raw::RawPayload>,
    {
        for row in rows.iter() {
            for blob in [row.attrs_json.as_deref(), row.resource_json.as_deref()]
                .into_iter()
                .flatten()
            {
                validate_public_attributes(blob)?;
            }
        }
        if let Some(store) = &self.local_raw_store {
            let mut bytes = 0;
            for row in rows.iter() {
                account_raw_query_bytes(&mut bytes, row.attrs_json.as_deref().map_or(0, str::len))?;
                account_raw_query_bytes(
                    &mut bytes,
                    row.resource_json.as_deref().map_or(0, str::len),
                )?;
            }
            let mut payloads: BTreeMap<String, Option<CachedRawPayload>> = BTreeMap::new();
            for row in rows {
                for reference in local_raw::row_references(row)? {
                    if let Some(cached) = payloads.get(&reference) {
                        if let Some(payload) = cached {
                            hydrate_cached_payload(&self.tenant_id, row, &payload.0, &mut bytes)?;
                        }
                        continue;
                    }
                    let object = match store.raw_object_get(&self.tenant_id, &reference)? {
                        Some(object) => Some(object),
                        None => self.fetch_raw(&reference).await?,
                    };
                    if let Some(object) = object {
                        local_raw::validate_row_context(&self.tenant_id, row, &object.context)?;
                        account_raw_query_bytes(&mut bytes, object.ciphertext_b64.len())?;
                        let payload = CachedRawPayload(decrypt(&object)?);
                        hydrate_cached_payload(&self.tenant_id, row, &payload.0, &mut bytes)?;
                        match store.raw_object_cache(&object) {
                            Ok(()) => {},
                            Err(gently_store::StoreError::RawCapacity) => tracing::warn!("encrypted raw cache byte budget reached; returning verified in-memory result"),
                            Err(error) => return Err(error.into()),
                        }
                        payloads.insert(reference, Some(payload));
                    } else {
                        payloads.insert(reference, None);
                    }
                }
            }
        }
        Ok(())
    }
}

const RAW_ALIASES: &[&str] = &[
    "prompt",
    "user",
    "user_prompt",
    "assistant",
    "assistant_message",
    "last_assistant_message",
    "tool_input",
    "tool_response",
    "input",
    "output",
    "tool_output",
    "tool_result",
    "response",
    "raw",
    "compact_instructions",
    "compact_summary",
    "custom_instructions",
    "tool_calls",
    "error",
    "error_details",
    "hook_payload",
    "message.delta",
    "instruction_file",
];
const SAFE_REASONS: &[&str] = &[
    "clear",
    "logout",
    "prompt_input_exit",
    "bypass_permissions_disabled",
    "other",
    "exit",
    "shutdown",
];

fn validate_public_attributes(blob: &str) -> Result<()> {
    use serde_json::Value;
    fn check_key(key: &str, value: &Value) -> Result<()> {
        let base = key.strip_prefix("gently.").unwrap_or(key);
        anyhow::ensure!(
            !key.ends_with(".sha256") && !RAW_ALIASES.contains(&base),
            "collector returned a prohibited plaintext raw field"
        );
        if base == "reason" {
            anyhow::ensure!(
                value
                    .as_str()
                    .is_some_and(|reason| SAFE_REASONS.contains(&reason)),
                "collector returned a prohibited plaintext raw field"
            );
        }
        Ok(())
    }
    fn visit(value: &Value, depth: usize) -> Result<()> {
        anyhow::ensure!(depth <= 64, "invalid collector attribute shape");
        match value {
            Value::Array(values) => {
                for child in values {
                    visit(child, depth + 1)?;
                }
            }
            Value::Object(object) => {
                for (key, child) in object {
                    check_key(key, child)?;
                    if key == "key" {
                        let attr_key = child.as_str().context("invalid collector attribute key")?;
                        let attr_value = object
                            .get("value")
                            .and_then(|value| value.get("stringValue"))
                            .unwrap_or(&Value::Null);
                        check_key(attr_key, attr_value)?;
                    }
                    visit(child, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let attrs: Value = crate::json_fidelity::parse(blob)
        .map_err(|_| anyhow::anyhow!("invalid collector attributes"))?;
    anyhow::ensure!(attrs.is_array(), "invalid collector attribute shape");
    visit(&attrs, 0)
}

#[cfg(test)]
mod raw_guard_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn collector_validation_preserves_literal_private_json_keys() {
        assert!(validate_public_attributes(
            r#"[{"key":"gently.synthetic_metadata","value":{"$serde_json::private::Number":"literal-key"}}]"#
        )
        .is_ok());
    }

    fn client() -> QueryClient {
        QueryClient {
            collector: crate::collector::CollectorClient::new(
                "https://synthetic.invalid",
                "fixture",
                "personal",
                1,
            )
            .unwrap(),
            tenant_id: "personal".into(),
            #[cfg(unix)]
            query_socket: None,
            #[cfg(unix)]
            broker_base: "https://synthetic.invalid".into(),
            #[cfg(unix)]
            timeout: std::time::Duration::from_secs(1),
            local_raw_store: None,
            raw_identity_path: None,
            raw_identities: OnceCell::new(),
        }
    }

    fn row() -> SpanRow {
        serde_json::from_value(json!({"span_id":"0123456789abcdef","trace_id":"trace","parent_span_id":null,"name":"turn","kind":1,
            "start_unix_nano":"1","end_unix_nano":"2","status":1,"session_id":"session","harness":"codex","tool_name":null,"tool_use_id":null,
            "resource_json":null,"attrs_json":null})).unwrap()
    }

    #[test]
    fn raw_resolution_bounds_trace_assembly_at_its_own_budget() {
        let dir = tempfile::tempdir().unwrap();
        let mut raw = client();
        raw.local_raw_store =
            Some(gently_store::Store::open(&dir.path().join("state.db")).unwrap());
        let wide = |id: usize| SpanRow {
            span_id: format!("{id:016x}"),
            attrs_json: Some("x".repeat(1024 * 1024)),
            ..row()
        };
        let page = |range: std::ops::Range<usize>| TracePage {
            rows: range.map(wide).collect(),
            next_cursor: Some("next".into()),
            complete: false,
        };
        // Nine 1 MiB rows fail on the first page with raw resolution, so no
        // further pages are fetched; without it the same page is accepted.
        let error = raw
            .trace_assembly("trace")
            .accept(page(0..9))
            .err()
            .unwrap();
        assert!(error.to_string().contains("8 MiB raw-resolution budget"));
        assert!(client().trace_assembly("trace").accept(page(0..9)).is_ok());
    }

    #[test]
    fn a_repeated_span_is_a_trace_change() {
        let mut assembly = client().trace_assembly("trace");
        let page = |cursor: &str| TracePage {
            rows: vec![row()],
            next_cursor: Some(cursor.into()),
            complete: false,
        };
        assert!(assembly.accept(page("first")).is_ok());
        assert!(is_trace_changed(
            &assembly.accept(page("second")).err().unwrap()
        ));
    }

    #[tokio::test]
    async fn shared_raw_ref_decrypts_once_per_request_and_rechecks_each_span_binding() {
        use gently_raw::{
            DeviceIdentity, Manifest, OwnerKey, RawContext, ReaderIdentities, Recipient, TrustPin,
            VerifiedManifest,
        };
        use std::collections::BTreeMap;
        let dir = tempfile::tempdir().unwrap();
        let store = gently_store::Store::open(&dir.path().join("state.db")).unwrap();
        let identity = DeviceIdentity::generate();
        let owner = OwnerKey::generate();
        let manifest = Manifest {
            version: 1,
            tenant_id: "personal".into(),
            key_epoch: 1,
            expires_unix_secs: 4_102_444_800,
            readers: vec![Recipient {
                device_id: "reader".into(),
                key_id: "key".into(),
                recipient: identity.to_public().to_string(),
            }],
        };
        let pin = TrustPin {
            tenant_id: "personal".into(),
            owner_verify_key_b64: owner.verification_key_b64(),
            min_epoch: 1,
            manifest_digest: gently_raw::manifest_digest(&manifest).unwrap(),
        };
        let verified =
            VerifiedManifest::verify(&gently_raw::sign_manifest(manifest, &owner).unwrap(), &pin)
                .unwrap();
        let context = RawContext {
            tenant_id: "personal".into(),
            device_id: "writer".into(),
            key_epoch: 1,
            raw_ref: gently_raw::new_raw_ref(),
            session_id: "session".into(),
            harness: "codex".into(),
            event: "UserPromptSubmit".into(),
        };
        let second_span = "fedcba9876543210";
        let object = gently_raw::seal(
            &verified,
            context.clone(),
            BTreeMap::from([("gently.prompt".into(), "private repeated fixture".into())]),
            BTreeMap::from([(
                "gently.prompt".into(),
                vec![row().span_id, second_span.into()],
            )]),
        )
        .unwrap();
        store.raw_object_cache(&object).unwrap();
        let identities = ReaderIdentities::from_native(vec![identity]);
        let mut first = row();
        first.resource_json = Some(
            json!([
                {"key":"gently.tenant_id","value":{"stringValue":"personal"}},
                {"key":"gently.device_id","value":{"stringValue":"writer"}}
            ])
            .to_string(),
        );
        first.attrs_json = Some(
            json!([{"key":"gently.prompt.raw_ref","value":{"stringValue":context.raw_ref}}])
                .to_string(),
        );
        let mut second = first.clone();
        second.span_id = second_span.into();
        let mut client = client();
        client.local_raw_store = Some(store);
        let mut decryptions = 0;
        let mut rows = vec![first.clone(), second];
        client
            .resolve_raw_values_with_reader(&mut rows, |object| {
                decryptions += 1;
                Ok(gently_raw::open(object, &object.context, &identities)?)
            })
            .await
            .unwrap();
        assert_eq!(decryptions, 1, "one authorization for a shared ciphertext");
        assert!(rows.iter().all(|row| row
            .attrs_json
            .as_ref()
            .unwrap()
            .contains("private repeated fixture")));
        let mut another_request = vec![first.clone()];
        client
            .resolve_raw_values_with_reader(&mut another_request, |object| {
                decryptions += 1;
                Ok(gently_raw::open(object, &object.context, &identities)?)
            })
            .await
            .unwrap();
        assert_eq!(decryptions, 2, "plaintext cache ends with each request");
        for mismatch in ["span", "tenant", "device", "session", "harness", "field"] {
            let mut unrelated = first.clone();
            match mismatch {
                "span" => unrelated.span_id = "0000000000000000".into(),
                "tenant" | "device" => {
                    unrelated.resource_json = unrelated.resource_json.map(|json| {
                        json.replace(
                            if mismatch == "tenant" {
                                "personal"
                            } else {
                                "writer"
                            },
                            "different",
                        )
                    })
                }
                "session" => unrelated.session_id = Some("different".into()),
                "harness" => unrelated.harness = Some("different".into()),
                "field" => {
                    unrelated.attrs_json = unrelated.attrs_json.map(|json| {
                        json.replace("gently.prompt.raw_ref", "gently.tool_input.raw_ref")
                    })
                }
                _ => unreachable!(),
            }
            let before = unrelated.attrs_json.clone();
            let previous = decryptions;
            let mut rows = vec![first.clone(), unrelated];
            assert!(
                client
                    .resolve_raw_values_with_reader(&mut rows, |object| {
                        decryptions += 1;
                        Ok(gently_raw::open(object, &object.context, &identities)?)
                    })
                    .await
                    .is_err(),
                "cached payload bypassed {mismatch}"
            );
            assert_eq!(decryptions, previous + 1);
            assert_eq!(
                rows[1].attrs_json, before,
                "shared ciphertext must not bypass {mismatch}"
            );
        }
    }

    #[tokio::test]
    async fn raw_resolution_bounds_aggregate_cached_objects_and_expanded_output() {
        use gently_raw::{
            DeviceIdentity, Manifest, OwnerKey, RawContext, ReaderIdentities, Recipient, TrustPin,
            VerifiedManifest,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = gently_store::Store::open(&dir.path().join("state.db")).unwrap();
        let identity = DeviceIdentity::generate();
        let owner = OwnerKey::generate();
        let manifest = Manifest {
            version: 1,
            tenant_id: "personal".into(),
            key_epoch: 1,
            expires_unix_secs: 4_102_444_800,
            readers: vec![Recipient {
                device_id: "reader".into(),
                key_id: "reader-key".into(),
                recipient: identity.to_public().to_string(),
            }],
        };
        let pin = TrustPin {
            tenant_id: "personal".into(),
            owner_verify_key_b64: owner.verification_key_b64(),
            min_epoch: 1,
            manifest_digest: gently_raw::manifest_digest(&manifest).unwrap(),
        };
        let policy =
            VerifiedManifest::verify(&gently_raw::sign_manifest(manifest, &owner).unwrap(), &pin)
                .unwrap();
        let identities = ReaderIdentities::from_native(vec![identity]);
        let mut rows = Vec::new();
        for _ in 0..40 {
            let mut row = row();
            let context = RawContext {
                tenant_id: "personal".into(),
                device_id: "writer".into(),
                key_epoch: 1,
                raw_ref: gently_raw::new_raw_ref(),
                session_id: "session".into(),
                harness: "codex".into(),
                event: "UserPromptSubmit".into(),
            };
            let object = gently_raw::seal(
                &policy,
                context.clone(),
                BTreeMap::from([("gently.prompt".into(), "x".repeat(128 * 1024))]),
                BTreeMap::from([("gently.prompt".into(), vec![row.span_id.clone()])]),
            )
            .unwrap();
            store.raw_object_cache(&object).unwrap();
            row.resource_json = Some(
                json!([
                    {"key":"gently.tenant_id","value":{"stringValue":"personal"}},
                    {"key":"gently.device_id","value":{"stringValue":"writer"}}
                ])
                .to_string(),
            );
            row.attrs_json = Some(
                json!([{"key":"gently.prompt.raw_ref","value":{"stringValue":context.raw_ref}}])
                    .to_string(),
            );
            rows.push(row);
        }
        let mut client = client();
        client.local_raw_store = Some(store);
        let error = client
            .resolve_raw_values_with_reader(&mut rows, |object| {
                Ok(gently_raw::open(object, &object.context, &identities)?)
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("budget; narrow your query"));
    }

    #[tokio::test]
    async fn collector_raw_aliases_are_rejected_in_both_resource_and_span_attributes() {
        let client = client();
        for key in [
            "reason",
            "prompt",
            "tool_input",
            "raw",
            "gently.reason",
            "gently.user_prompt",
            "gently.assistant_message",
            "gently.last_assistant_message",
            "gently.custom_instructions",
            "gently.compact_summary",
            "gently.tool_calls",
            "gently.error_details",
            "gently.error",
            "gently.input",
            "gently.output",
            "arbitrary.sha256",
        ] {
            for resource in [false, true] {
                let mut row = row();
                let attrs = Some(
                    json!([{"key":key,"value":{"stringValue":"synthetic-private-canary"}}])
                        .to_string(),
                );
                if resource {
                    row.resource_json = attrs;
                } else {
                    row.attrs_json = attrs;
                }
                let before = row.clone();
                let error = client
                    .resolve_raw_values(std::slice::from_mut(&mut row))
                    .await
                    .unwrap_err();
                assert!(!error.to_string().contains("synthetic-private-canary"));
                assert_eq!(row.attrs_json, before.attrs_json);
                assert_eq!(row.resource_json, before.resource_json);
            }
        }
    }

    #[tokio::test]
    async fn documented_reason_enums_lengths_and_refs_remain_public_metadata() {
        let client = client();
        for reason in [
            "clear",
            "logout",
            "prompt_input_exit",
            "bypass_permissions_disabled",
            "other",
            "exit",
            "shutdown",
        ] {
            let mut row = row();
            let attrs = json!([
                {"key":"gently.reason","value":{"stringValue":reason}},
                {"key":"gently.reason.bytes","value":{"stringValue":"12"}},
                {"key":"gently.prompt.raw_ref","value":{"stringValue":"00000000000000000000000000000001"}}
            ]).to_string();
            row.resource_json = Some(attrs.clone());
            row.attrs_json = Some(attrs);
            client
                .resolve_raw_values(std::slice::from_mut(&mut row))
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn reader_that_cannot_decrypt_is_reported_as_a_reader_failure() {
        use gently_raw::{
            DeviceIdentity, Manifest, OwnerKey, RawContext, ReaderIdentities, Recipient, TrustPin,
            VerifiedManifest,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = gently_store::Store::open(&dir.path().join("state.db")).unwrap();
        let enrolled = DeviceIdentity::generate();
        let owner = OwnerKey::generate();
        let manifest = Manifest {
            version: 1,
            tenant_id: "personal".into(),
            key_epoch: 1,
            expires_unix_secs: 4_102_444_800,
            readers: vec![Recipient {
                device_id: "reader".into(),
                key_id: "key".into(),
                recipient: enrolled.to_public().to_string(),
            }],
        };
        let pin = TrustPin {
            tenant_id: "personal".into(),
            owner_verify_key_b64: owner.verification_key_b64(),
            min_epoch: 1,
            manifest_digest: gently_raw::manifest_digest(&manifest).unwrap(),
        };
        let verified =
            VerifiedManifest::verify(&gently_raw::sign_manifest(manifest, &owner).unwrap(), &pin)
                .unwrap();
        let context = RawContext {
            tenant_id: "personal".into(),
            device_id: "writer".into(),
            key_epoch: 1,
            raw_ref: gently_raw::new_raw_ref(),
            session_id: "session".into(),
            harness: "codex".into(),
            event: "UserPromptSubmit".into(),
        };
        let mut row = row();
        let object = gently_raw::seal(
            &verified,
            context.clone(),
            BTreeMap::from([("gently.prompt".into(), "private older fixture".into())]),
            BTreeMap::from([("gently.prompt".into(), vec![row.span_id.clone()])]),
        )
        .unwrap();
        store.raw_object_cache(&object).unwrap();
        row.resource_json = Some(
            json!([
                {"key":"gently.tenant_id","value":{"stringValue":"personal"}},
                {"key":"gently.device_id","value":{"stringValue":"writer"}}
            ])
            .to_string(),
        );
        row.attrs_json = Some(
            json!([{"key":"gently.prompt.raw_ref","value":{"stringValue":context.raw_ref}}])
                .to_string(),
        );
        let mut client = client();
        client.local_raw_store = Some(store);
        // A newer reader is not a recipient of objects sealed before it enrolled.
        let newer = ReaderIdentities::from_native(vec![DeviceIdentity::generate()]);
        assert!(client.raw_identities.set(newer).is_ok());
        let error = client
            .resolve_raw_values(std::slice::from_mut(&mut row))
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<QueryFailure>(),
            Some(QueryFailure::RawDecrypt)
        ));
        assert!(!error.to_string().contains("private older fixture"));
    }

    #[tokio::test]
    async fn multiple_inherited_refs_hydrate_once_without_rejecting_verified_plaintext() {
        use gently_raw::{
            DeviceIdentity, Manifest, OwnerKey, RawContext, ReaderIdentities, Recipient, TrustPin,
            VerifiedManifest,
        };
        use std::collections::BTreeMap;

        let dir = tempfile::tempdir().unwrap();
        let store = gently_store::Store::open(&dir.path().join("state.db")).unwrap();
        let identity = DeviceIdentity::generate();
        let owner = OwnerKey::generate();
        let manifest = Manifest {
            version: 1,
            tenant_id: "personal".into(),
            key_epoch: 1,
            expires_unix_secs: 4_102_444_800,
            readers: vec![Recipient {
                device_id: "reader".into(),
                key_id: "key".into(),
                recipient: identity.to_public().to_string(),
            }],
        };
        let pin = TrustPin {
            tenant_id: "personal".into(),
            owner_verify_key_b64: owner.verification_key_b64(),
            min_epoch: 1,
            manifest_digest: gently_raw::manifest_digest(&manifest).unwrap(),
        };
        let verified =
            VerifiedManifest::verify(&gently_raw::sign_manifest(manifest, &owner).unwrap(), &pin)
                .unwrap();
        let mut row = row();
        row.resource_json = Some(
            json!([
                {"key":"gently.tenant_id","value":{"stringValue":"personal"}},
                {"key":"gently.device_id","value":{"stringValue":"writer"}}
            ])
            .to_string(),
        );
        let mut refs = Vec::new();
        for (field, event, content) in [
            ("gently.tool_input", "PreToolUse", "private input fixture"),
            (
                "gently.tool_response",
                "PostToolUse",
                "private output fixture",
            ),
        ] {
            let context = RawContext {
                tenant_id: "personal".into(),
                device_id: "writer".into(),
                key_epoch: 1,
                raw_ref: gently_raw::new_raw_ref(),
                session_id: "session".into(),
                harness: "codex".into(),
                event: event.into(),
            };
            refs.push(
                json!({"key":format!("{field}.raw_ref"),"value":{"stringValue":context.raw_ref}}),
            );
            let object = gently_raw::seal(
                &verified,
                context,
                BTreeMap::from([(field.into(), content.into())]),
                BTreeMap::from([(field.into(), vec![row.span_id.clone()])]),
            )
            .unwrap();
            store.raw_object_cache(&object).unwrap();
        }
        row.attrs_json = Some(serde_json::to_string(&refs).unwrap());
        let mut client = client();
        client.local_raw_store = Some(store);
        assert!(client
            .raw_identities
            .set(ReaderIdentities::from_native(vec![identity]))
            .is_ok());
        client
            .resolve_raw_values(std::slice::from_mut(&mut row))
            .await
            .unwrap();
        let attrs: serde_json::Value =
            serde_json::from_str(row.attrs_json.as_deref().unwrap()).unwrap();
        for (field, content) in [
            ("gently.tool_input", "private input fixture"),
            ("gently.tool_response", "private output fixture"),
        ] {
            assert!(attrs
                .as_array()
                .unwrap()
                .iter()
                .any(|attr| attr["key"] == field && attr["value"]["stringValue"] == content));
        }
        assert_eq!(local_raw::row_references(&row).unwrap().len(), 2);
        assert_eq!(
            client
                .local_raw_store
                .as_ref()
                .unwrap()
                .raw_objects_len()
                .unwrap(),
            2
        );
        assert_eq!(
            client
                .local_raw_store
                .as_ref()
                .unwrap()
                .raw_objects_pending("personal", 10)
                .unwrap()
                .len(),
            0
        );
        for suffix in ["", "-wal"] {
            let bytes = std::fs::read(format!("{}{suffix}", dir.path().join("state.db").display()))
                .unwrap();
            assert!(!bytes
                .windows(b"private input fixture".len())
                .any(|w| w == b"private input fixture"));
            assert!(!bytes
                .windows(b"private output fixture".len())
                .any(|w| w == b"private output fixture"));
        }
    }
}
