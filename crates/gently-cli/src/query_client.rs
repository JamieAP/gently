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
    collector: crate::collector::CollectorClient,
    tenant_id: String,
    local_raw_store: Option<gently_store::Store>,
    raw_identities: Option<gently_raw::ReaderIdentities>,
}

impl QueryClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        cfg.require_collector()?;
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
            raw_identities: if cfg.resolve_raw_values {
                let path = cfg
                    .raw_identity
                    .as_deref()
                    .context("raw resolution requires an explicit reader identity")?;
                gently_store::private_fs::harden_existing_file(path)?;
                Some(gently_raw::load_identities(path)?)
            } else {
                None
            },
            local_raw_store,
        })
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, params: &[(&str, String)]) -> Result<T> {
        self.collector.query(params).await
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
        for row in rows.iter() {
            for blob in [row.attrs_json.as_deref(), row.resource_json.as_deref()]
                .into_iter()
                .flatten()
            {
                validate_public_attributes(blob)?;
            }
        }
        if let (Some(store), Some(identities)) = (&self.local_raw_store, &self.raw_identities) {
            for row in rows {
                for reference in local_raw::row_references(row)? {
                    let object = match store.raw_object_get(&self.tenant_id, &reference)? {
                        Some(object) => Some(object),
                        None => self.collector.fetch_raw(&reference).await?,
                    };
                    if let Some(object) = object {
                        local_raw::resolve_row(&self.tenant_id, row, &object, identities)?;
                        match store.raw_object_cache(&object) {
                            Ok(()) => {},
                            Err(gently_store::StoreError::RawCapacity) => tracing::warn!("encrypted raw cache byte budget reached; returning verified in-memory result"),
                            Err(error) => return Err(error.into()),
                        }
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
    let attrs: Value =
        serde_json::from_str(blob).map_err(|_| anyhow::anyhow!("invalid collector attributes"))?;
    anyhow::ensure!(attrs.is_array(), "invalid collector attribute shape");
    visit(&attrs, 0)
}

#[cfg(test)]
mod raw_guard_tests {
    use super::*;
    use serde_json::json;

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
            local_raw_store: None,
            raw_identities: None,
        }
    }

    fn row() -> SpanRow {
        serde_json::from_value(json!({"span_id":"0123456789abcdef","trace_id":"trace","parent_span_id":null,"name":"turn","kind":1,
            "start_unix_nano":"1","end_unix_nano":"2","status":1,"session_id":"session","harness":"codex","tool_name":null,"tool_use_id":null,
            "resource_json":null,"attrs_json":null})).unwrap()
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
        client.raw_identities = Some(ReaderIdentities::from_native(vec![identity]));
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
