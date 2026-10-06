//! Leaf helpers shared by the stdin-JSON hook adapters (Claude Code, Codex).
//!
//! Codex's hook protocol is a near-clone of Claude's - same stdin-JSON shape and
//! field names - so field extraction, content length recording, the common attribute
//! set, and the unknown-event marker are identical and live here. The per-harness
//! event vocabularies differ, so each adapter keeps its own `parse` match.

use crate::Attrs;

/// Attributes present on every hook event: the event name plus optional model,
/// session source, permission mode and agent type when the payload carries them.
pub(crate) fn common_attrs(raw: &serde_json::Value, event: &str) -> Attrs {
    let mut attrs = vec![("gently.event".to_string(), event.to_string())];
    // `model` is a common field on every Codex hook event (≥0.136) and is the
    // only in-band source of the model that produced the work - captured here so
    // a model breakdown is queryable straight from a span, without parsing
    // transcripts. Current Claude SessionStart can also supply an optional
    // model; events that omit it leave the attribute unset.
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
    // Preserve only known lifecycle enums; measure arbitrary text.
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
            push_length(&mut attrs, "gently.reason", reason.as_bytes());
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
    push_first_str_length(
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

/// A permission observation refers to a tool without being an execution span.
/// Namespace correlation metadata so collector tool rollups stay accurate.
pub(crate) fn push_observed_tool_attrs(attrs: &mut Attrs, raw: &serde_json::Value) {
    for field in ["tool_name", "tool_use_id"] {
        if let Some(value) = str_field(raw, field).filter(|v| !v.is_empty()) {
            attrs.push((format!("gently.hook.{field}"), value));
        }
    }
    if let Some(input) = raw.get("tool_input") {
        push_value_length(attrs, "gently.tool_input", input);
    }
}

/// Append a `<key>.bytes` attribute for an arbitrary JSON value.
/// Content fingerprints permit guesses and are never recorded.
pub(crate) fn push_value_length(attrs: &mut Attrs, key: &str, value: &serde_json::Value) {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    push_length(attrs, key, &bytes);
}

pub(crate) fn push_first_str_length(
    attrs: &mut Attrs,
    key: &str,
    raw: &serde_json::Value,
    fields: &[&str],
) -> bool {
    for field in fields {
        if let Some(value) = str_field(raw, field) {
            push_length(attrs, key, value.as_bytes());
            return true;
        }
    }
    false
}

pub(crate) fn push_length(attrs: &mut Attrs, key: &str, bytes: &[u8]) {
    attrs.push((format!("{key}.bytes"), bytes.len().to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn freeform_permission_reason_has_only_a_length() {
        let attrs = common_attrs(
            &serde_json::json!({"reason":"secret command content"}),
            "PermissionDenied",
        );
        assert!(attrs.iter().any(|(k, _)| k == "gently.reason.bytes"));
        assert!(!attrs
            .iter()
            .any(|(_, v)| v.contains("secret command content")));
    }
}
