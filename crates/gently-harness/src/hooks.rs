//! Leaf helpers shared by the stdin-JSON hook adapters (Claude Code, Codex).
//!
//! Codex's hook protocol is a near-clone of Claude's - same stdin-JSON shape and
//! field names - so field extraction, content digesting, the common attribute
//! set, and the unknown-event marker are identical and live here. The per-harness
//! event vocabularies differ, so each adapter keeps its own `parse` match.

use crate::{Attrs, SpanOp};
use sha2::{Digest, Sha256};

/// An unmodeled or point-in-time event, recorded as an instant marker span.
pub(crate) fn mark(raw: &serde_json::Value, event: &str) -> SpanOp {
    SpanOp::Mark {
        name: event.to_string(),
        attrs: common_attrs(raw, event),
    }
}

/// Attributes present on every hook event: the event name plus optional
/// permission mode and agent type when the payload carries them.
pub(crate) fn common_attrs(raw: &serde_json::Value, event: &str) -> Attrs {
    let mut attrs = vec![("gently.event".to_string(), event.to_string())];
    if let Some(pm) = str_field(raw, "permission_mode") {
        attrs.push(("gently.permission_mode".into(), pm));
    }
    if let Some(at) = str_field(raw, "agent_type") {
        attrs.push(("gently.agent_type".into(), at));
    }
    push_first_str_digest(
        &mut attrs,
        "gently.assistant",
        raw,
        &["last_assistant_message", "assistant", "assistant_message"],
    );
    attrs
}

pub(crate) fn str_field(raw: &serde_json::Value, key: &str) -> Option<String> {
    raw.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

pub(crate) fn u64_field(raw: &serde_json::Value, key: &str) -> Option<u64> {
    raw.get(key).and_then(serde_json::Value::as_u64)
}

/// Append a `<key>.sha256` (first 16 hex chars) and `<key>.bytes` attribute for
/// an arbitrary JSON value, never the value itself.
pub(crate) fn push_value_digest(attrs: &mut Attrs, key: &str, value: &serde_json::Value) {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    push_digest(attrs, key, &bytes);
}

pub(crate) fn push_first_str_digest(
    attrs: &mut Attrs,
    key: &str,
    raw: &serde_json::Value,
    fields: &[&str],
) -> bool {
    for field in fields {
        if let Some(value) = str_field(raw, field) {
            push_digest(attrs, key, value.as_bytes());
            return true;
        }
    }
    false
}

pub(crate) fn push_digest(attrs: &mut Attrs, key: &str, bytes: &[u8]) {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    attrs.push((format!("{key}.sha256"), hex));
    attrs.push((format!("{key}.bytes"), bytes.len().to_string()));
}
