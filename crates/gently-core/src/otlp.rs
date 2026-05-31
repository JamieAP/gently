//! OTLP/JSON wire types.
//!
//! Per the OTLP/JSON spec, all 64-bit integer fields (the `*UnixNano` times and
//! `intValue`) are encoded as **strings**, and trace/span ids are lowercase hex
//! strings. The serde structs here enforce that so the Worker can parse them
//! without JS number-precision loss.

use crate::span::{Resource, Span};
use serde::{Deserialize, Serialize};

/// Top-level OTLP trace export request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpRequest {
    #[serde(rename = "resourceSpans")]
    pub resource_spans: Vec<ResourceSpans>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceSpans {
    pub resource: OtlpResource,
    #[serde(rename = "scopeSpans")]
    pub scope_spans: Vec<ScopeSpans>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpResource {
    pub attributes: Vec<KeyValue>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScopeSpans {
    pub scope: Scope,
    pub spans: Vec<OtlpSpan>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scope {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpSpan {
    #[serde(rename = "traceId")]
    pub trace_id: String,
    #[serde(rename = "spanId")]
    pub span_id: String,
    #[serde(rename = "parentSpanId", skip_serializing_if = "String::is_empty", default)]
    pub parent_span_id: String,
    pub name: String,
    pub kind: u8,
    #[serde(rename = "startTimeUnixNano")]
    pub start_time_unix_nano: String,
    #[serde(rename = "endTimeUnixNano")]
    pub end_time_unix_nano: String,
    #[serde(default)]
    pub attributes: Vec<KeyValue>,
    pub status: SpanStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpanStatus {
    #[serde(default)]
    pub code: u8,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyValue {
    pub key: String,
    pub value: AnyValue,
}

/// OTLP `AnyValue`. We only emit string and int values.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnyValue {
    #[serde(rename = "stringValue", skip_serializing_if = "Option::is_none", default)]
    pub string_value: Option<String>,
    #[serde(rename = "intValue", skip_serializing_if = "Option::is_none", default)]
    pub int_value: Option<String>,
}

impl AnyValue {
    fn string(s: impl Into<String>) -> Self {
        Self { string_value: Some(s.into()), int_value: None }
    }
}

impl KeyValue {
    fn string(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self { key: key.into(), value: AnyValue::string(value) }
    }
}

const SCOPE_NAME: &str = "gently";

impl OtlpRequest {
    /// Build a request for one resource and its spans.
    pub fn single(resource: &Resource, spans: Vec<Span>) -> Self {
        let otlp_spans = spans.into_iter().map(OtlpSpan::from).collect();
        Self {
            resource_spans: vec![ResourceSpans {
                resource: OtlpResource::from(resource),
                scope_spans: vec![ScopeSpans {
                    scope: Scope {
                        name: SCOPE_NAME.to_string(),
                        version: env!("CARGO_PKG_VERSION").to_string(),
                    },
                    spans: otlp_spans,
                }],
            }],
        }
    }

    /// Merge several requests into one by concatenating their resource groups.
    /// Used by the exporter to coalesce a batch of single-span outbox rows into
    /// one wire request.
    pub fn merge(requests: Vec<OtlpRequest>) -> Self {
        Self {
            resource_spans: requests
                .into_iter()
                .flat_map(|r| r.resource_spans)
                .collect(),
        }
    }

    /// Total spans across all resource/scope groups.
    pub fn span_count(&self) -> usize {
        self.resource_spans
            .iter()
            .flat_map(|rs| rs.scope_spans.iter())
            .map(|ss| ss.spans.len())
            .sum()
    }
}

impl From<&Resource> for OtlpResource {
    fn from(r: &Resource) -> Self {
        Self {
            attributes: vec![
                KeyValue::string("service.name", "gently"),
                KeyValue::string("gently.harness", &r.harness),
                KeyValue::string("gently.session_id", &r.session_id),
                KeyValue::string("gently.cwd", &r.cwd),
                KeyValue::string("host.name", &r.host),
                KeyValue::string("os.type", &r.os),
                KeyValue::string("gently.version", &r.version),
            ],
        }
    }
}

impl From<Span> for OtlpSpan {
    fn from(s: Span) -> Self {
        Self {
            trace_id: s.trace_id.to_hex(),
            span_id: s.span_id.to_hex(),
            parent_span_id: s.parent_span_id.map(|p| p.to_hex()).unwrap_or_default(),
            name: s.name,
            kind: s.kind.as_otlp(),
            start_time_unix_nano: s.start_unix_nano.to_string(),
            end_time_unix_nano: s.end_unix_nano.to_string(),
            attributes: s
                .attributes
                .into_iter()
                .map(|(k, v)| KeyValue::string(k, v))
                .collect(),
            status: SpanStatus {
                code: s.status.code(),
                message: s.status.message().map(str::to_string),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{SpanId, TraceId};
    use crate::span::{SpanKind, Status};

    fn sample_span() -> Span {
        Span {
            trace_id: TraceId::from_session("s"),
            span_id: SpanId::derive("s", "turn:1"),
            parent_span_id: None,
            name: "turn:1".into(),
            kind: SpanKind::Internal,
            start_unix_nano: 1_700_000_000_000_000_000,
            end_unix_nano: 1_700_000_000_500_000_000,
            status: Status::Unset,
            attributes: vec![("gently.event".into(), "UserPromptSubmit".into())],
        }
    }

    #[test]
    fn otlp_json_encodes_uint64_and_ids_as_strings() {
        let res = Resource::new("sess-1", "claude-code", "/tmp");
        let req = OtlpRequest::single(&res, vec![sample_span()]);
        let json = serde_json::to_value(&req).unwrap();
        let s = &json["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(s["startTimeUnixNano"], "1700000000000000000");
        assert_eq!(s["endTimeUnixNano"], "1700000000500000000");
        assert_eq!(s["traceId"].as_str().unwrap().len(), 32);
        assert_eq!(s["spanId"].as_str().unwrap().len(), 16);

        let back: OtlpRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.span_count(), 1);
    }

    #[test]
    fn parent_span_id_omitted_when_absent() {
        let res = Resource::new("s", "claude-code", "/tmp");
        let req = OtlpRequest::single(&res, vec![sample_span()]);
        let json = serde_json::to_value(&req).unwrap();
        let s = &json["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert!(s.get("parentSpanId").is_none());
    }
}
