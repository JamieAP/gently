//! Leaf helpers shared by the stdin-JSON hook adapters (Claude Code, Codex).
//!
//! Codex's hook protocol is a near-clone of Claude's - same stdin-JSON shape and
//! field names - so field extraction, content digesting, the common attribute
//! set, and the unknown-event marker are identical and live here. The per-harness
//! event vocabularies differ, so each adapter keeps its own `parse` match.

use crate::Attrs;
use sha2::{Digest, Sha256};

/// Attributes present on every hook event: the event name plus optional model,
/// session source, permission mode and agent type when the payload carries them.
pub(crate) fn common_attrs(raw: &serde_json::Value, event: &str) -> Attrs {
    let mut attrs = vec![("gently.event".to_string(), event.to_string())];
    // `model` is a common field on every Codex hook event (≥0.136) and is the
    // only in-band source of the model that produced the work - captured here so
    // a model breakdown is queryable straight from a span, without parsing
    // transcripts. Claude omits it from the payload, so this is a no-op there.
    if let Some(model) = str_field(raw, "model") {
        attrs.push(("gently.model".into(), model));
    }
    // SessionStart carries `source` (startup|resume|clear|compact); lets a query
    // distinguish a fresh start from a resume/compaction continuation.
    if let Some(source) = str_field(raw, "source") {
        attrs.push(("gently.source".into(), source));
    }
    // Claude tags tool/turn events with the active effort level (the
    // effort/fast-mode dial) - captured so a breakdown can split work by it.
    // The field is an object `{"level": "medium"}`; tolerate a bare string too.
    if let Some(level) = raw.get("effort").and_then(|e| {
        e.get("level")
            .and_then(|l| l.as_str())
            .or_else(|| e.as_str())
    }) {
        attrs.push(("gently.effort".into(), level.to_string()));
    }
    // Other events may use freeform reasons containing command or user text.
    // Preserve only known lifecycle enums; hash arbitrary text.
    if let Some(reason) = str_field(raw, "reason") {
        if event == "SessionEnd"
            && matches!(
                reason.as_str(),
                "clear"
                    | "logout"
                    | "prompt_input_exit"
                    | "bypass_permissions_disabled"
                    | "other"
                    | "exit"
                    | "shutdown"
            )
        {
            attrs.push(("gently.reason".into(), reason));
        } else {
            push_digest(&mut attrs, "gently.reason", reason.as_bytes());
        }
    }
    // SubagentStop carries `agent_transcript_path` - the path to the subagent's
    // own transcript, so a subagent span can be traced back to its full log.
    if let Some(p) = str_field(raw, "agent_transcript_path") {
        attrs.push(("gently.agent_transcript_path".into(), p));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn freeform_permission_reason_is_digested() {
        let attrs = common_attrs(
            &serde_json::json!({"reason":"secret command content"}),
            "PermissionDenied",
        );
        assert!(attrs.iter().any(|(k, _)| k == "gently.reason.sha256"));
        assert!(!attrs
            .iter()
            .any(|(_, v)| v.contains("secret command content")));
    }
}
