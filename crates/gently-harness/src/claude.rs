//! Claude Code hook adapter.
//!
//! Maps the documented Claude Code hook events onto [`SpanOp`]s. The mapping is
//! deliberately tolerant: only `session_id` and `hook_event_name` are required;
//! events we do not model become [`SpanOp::Mark`]s, so the
//! adapter is forward-compatible by construction. No raw prompt or tool content
//! is ever placed in a span - only a digest and byte length.

use crate::hooks::{common_attrs, push_first_str_digest, push_value_digest, str_field, u64_field};
use crate::{Attrs, Harness, HarnessError, Parsed, SpanOp};
use gently_core::Status;

/// The Claude Code harness adapter.
pub struct ClaudeCode;

impl Harness for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn parse(&self, raw: &serde_json::Value) -> Result<Parsed, HarnessError> {
        let event_owned = str_field(raw, "hook_event_name")
            .ok_or(HarnessError::MissingField("hook_event_name"))?;
        let event = event_owned.as_str();
        let session_id =
            str_field(raw, "session_id").ok_or(HarnessError::MissingField("session_id"))?;
        let cwd = str_field(raw, "cwd").unwrap_or_default();
        // Authoritative session file, handed to the hook on every event - kept
        // verbatim so a consumer can read the transcript without slug-guessing.
        let transcript_path = str_field(raw, "transcript_path");
        // Claude ≥2.1.196 supplies a prompt UUID on every event during a turn.
        // Older payloads keep the applier's monotonic-counter fallback.
        let turn_id = str_field(raw, "prompt_id").filter(|id| !id.is_empty());
        // On lifecycle hooks agent_id names the child being started/stopped.
        // On ordinary hooks it identifies the subagent executing this event.
        let agent_id = if matches!(event, "SubagentStart" | "SubagentStop") {
            None
        } else {
            str_field(raw, "agent_id").filter(|id| !id.is_empty())
        };

        let ops = match event {
            "SessionStart" => vec![SpanOp::OpenSession {
                attrs: claude_attrs(raw, event),
            }],
            "SessionEnd" => vec![SpanOp::CloseSession {
                status: Status::Ok,
                attrs: claude_attrs(raw, event),
            }],
            "UserPromptSubmit" => {
                let mut attrs = claude_attrs(raw, event);
                push_first_str_digest(
                    &mut attrs,
                    "gently.prompt",
                    raw,
                    &["prompt", "user_prompt", "user"],
                );
                vec![SpanOp::OpenTurn { attrs }]
            }
            "Stop" => vec![SpanOp::CloseTurn {
                status: Status::Ok,
                attrs: claude_attrs(raw, event),
            }],
            "StopFailure" => vec![SpanOp::CloseTurn {
                status: Status::Error(None),
                attrs: claude_attrs(raw, event),
            }],
            "PreToolUse" => {
                let tool_name = str_field(raw, "tool_name").unwrap_or_default();
                let mut attrs = claude_attrs(raw, event);
                attrs.push(("gently.tool_name".into(), tool_name.clone()));
                if let Some(input) = raw.get("tool_input") {
                    push_value_digest(&mut attrs, "gently.tool_input", input);
                }
                vec![SpanOp::OpenTool {
                    tool_use_id: str_field(raw, "tool_use_id"),
                    tool_name,
                    attrs,
                }]
            }
            "PostToolUse" | "PostToolUseFailure" => {
                let tool_name = str_field(raw, "tool_name").unwrap_or_default();
                let mut attrs = claude_attrs(raw, event);
                attrs.push(("gently.tool_name".into(), tool_name.clone()));
                if let Some(resp) = raw.get("tool_response") {
                    push_value_digest(&mut attrs, "gently.tool_response", resp);
                }
                let status = if event == "PostToolUseFailure" {
                    Status::Error(None)
                } else {
                    Status::Ok
                };
                vec![SpanOp::CloseTool {
                    tool_use_id: str_field(raw, "tool_use_id"),
                    tool_name,
                    status,
                    duration_ms: u64_field(raw, "duration_ms"),
                    attrs,
                }]
            }
            "SubagentStart" => {
                let Some(agent_id) = str_field(raw, "agent_id").filter(|id| !id.is_empty()) else {
                    return Ok(Parsed {
                        session_id,
                        cwd,
                        transcript_path,
                        turn_id: turn_id.clone(),
                        agent_id: agent_id.clone(),
                        ops: vec![claude_mark(raw, event)],
                    });
                };
                vec![SpanOp::OpenAgent {
                    agent_id,
                    parent_tool_use_id: str_field(raw, "tool_use_id"),
                    attrs: claude_attrs(raw, event),
                }]
            }
            "SubagentStop" => {
                let Some(agent_id) = str_field(raw, "agent_id").filter(|id| !id.is_empty()) else {
                    return Ok(Parsed {
                        session_id,
                        cwd,
                        transcript_path,
                        turn_id: turn_id.clone(),
                        agent_id: agent_id.clone(),
                        ops: vec![claude_mark(raw, event)],
                    });
                };
                vec![SpanOp::CloseAgent {
                    agent_id,
                    status: Status::Ok,
                    attrs: claude_attrs(raw, event),
                }]
            }
            _ => vec![claude_mark(raw, event)],
        };

        Ok(Parsed {
            session_id,
            cwd,
            transcript_path,
            turn_id: turn_id.clone(),
            agent_id: agent_id.clone(),
            ops,
        })
    }
}

/// Current Claude-only metadata. Content-bearing fields are hashed, never copied.
fn claude_attrs(raw: &serde_json::Value, event: &str) -> Attrs {
    let mut attrs = common_attrs(raw, event);
    for field in ["prompt_id", "agent_id"] {
        if let Some(value) = str_field(raw, field).filter(|value| !value.is_empty()) {
            attrs.push((format!("gently.{field}"), value));
        }
    }
    match event {
        "PreCompact" | "PostCompact" => {
            if let Some(trigger) = str_field(raw, "trigger")
                .filter(|trigger| matches!(trigger.as_str(), "manual" | "auto"))
            {
                attrs.push(("gently.trigger".into(), trigger));
            }
            let (field, key) = if event == "PreCompact" {
                ("custom_instructions", "gently.compact_instructions")
            } else {
                ("compact_summary", "gently.compact_summary")
            };
            push_first_str_digest(&mut attrs, key, raw, &[field]);
        }
        "PostToolBatch" => {
            if let Some(calls) = raw.get("tool_calls").filter(|value| value.is_array()) {
                attrs.push((
                    "gently.tool_calls.count".into(),
                    calls.as_array().unwrap().len().to_string(),
                ));
                // Batch responses are serialized tool_result content, whereas
                // PostToolUse responses are structured outputs. Do not replay
                // tool closes: each tool's own post hook has already done that.
                push_value_digest(&mut attrs, "gently.tool_calls", calls);
            }
        }
        "StopFailure" => {
            if let Some(error) = str_field(raw, "error") {
                // Only documented machine-readable categories are safe to keep
                // verbatim. Future/freeform failures still retain a digest.
                if matches!(
                    error.as_str(),
                    "rate_limit"
                        | "overloaded"
                        | "authentication_failed"
                        | "oauth_org_not_allowed"
                        | "account_on_hold"
                        | "billing_error"
                        | "invalid_request"
                        | "model_not_found"
                        | "server_error"
                        | "max_output_tokens"
                        | "cloud_credential_error"
                        | "unknown"
                ) {
                    attrs.push(("gently.error_type".into(), error));
                }
            }
            push_first_str_digest(&mut attrs, "gently.error", raw, &["error"]);
            if let Some(details) = raw.get("error_details").filter(|value| !value.is_null()) {
                push_value_digest(&mut attrs, "gently.error_details", details);
            }
        }
        "PostToolUseFailure" => {
            // Tool errors can contain full stdout/stderr, not just an error code.
            push_first_str_digest(&mut attrs, "gently.error", raw, &["error"]);
            if let Some(interrupted) = raw.get("is_interrupt").and_then(|value| value.as_bool()) {
                attrs.push(("gently.is_interrupt".into(), interrupted.to_string()));
            }
        }
        "PreModelSwitch" | "PostModelSwitch" => {
            for field in ["from_model", "to_model"] {
                if let Some(model) = str_field(raw, field) {
                    attrs.push((format!("gently.{field}"), model.clone()));
                    if field == "to_model" && event == "PostModelSwitch" {
                        attrs.retain(|(key, _)| key != "gently.model");
                        attrs.push(("gently.model".into(), model));
                    }
                }
            }
        }
        _ => {}
    }
    attrs
}

fn claude_mark(raw: &serde_json::Value, event: &str) -> SpanOp {
    SpanOp::Mark {
        name: event.to_string(),
        attrs: claude_attrs(raw, event),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn pre_then_post_tool_use_pairs_via_tool_use_id() {
        let h = ClaudeCode;
        let pre = json!({"hook_event_name":"PreToolUse","session_id":"s",
            "cwd":"/w","tool_name":"Bash","tool_use_id":"tu_1",
            "tool_input":{"command":"ls"}});
        let parsed = h.parse(&pre).unwrap();
        assert_eq!(parsed.session_id, "s");
        match &parsed.ops[..] {
            [SpanOp::OpenTool {
                tool_use_id,
                tool_name,
                ..
            }] => {
                assert_eq!(tool_use_id.as_deref(), Some("tu_1"));
                assert_eq!(tool_name, "Bash");
            }
            other => panic!("expected OpenTool, got {other:?}"),
        }

        let post = json!({"hook_event_name":"PostToolUse","session_id":"s",
            "cwd":"/w","tool_name":"Bash","tool_use_id":"tu_1",
            "tool_response":{"stdout":"a"},"duration_ms":42});
        let parsed = h.parse(&post).unwrap();
        match &parsed.ops[..] {
            [SpanOp::CloseTool {
                tool_use_id,
                duration_ms,
                status,
                ..
            }] => {
                assert_eq!(tool_use_id.as_deref(), Some("tu_1"));
                assert_eq!(*duration_ms, Some(42));
                assert_eq!(*status, Status::Ok);
            }
            other => panic!("expected CloseTool, got {other:?}"),
        }
    }

    #[test]
    fn user_prompt_opens_turn_with_digest_not_content() {
        let parsed = ClaudeCode
            .parse(
                &json!({"hook_event_name":"UserPromptSubmit","session_id":"s",
                "prompt":"secret content here"}),
            )
            .unwrap();
        match &parsed.ops[..] {
            [SpanOp::OpenTurn { attrs }] => {
                let joined = format!("{attrs:?}");
                assert!(joined.contains("gently.prompt.sha256"));
                assert!(joined.contains("gently.prompt.bytes"));
                assert!(!joined.contains("secret content"));
            }
            other => panic!("expected OpenTurn, got {other:?}"),
        }
    }

    #[test]
    fn unknown_event_becomes_mark() {
        let parsed = ClaudeCode
            .parse(&json!({"hook_event_name":"TeammateIdle","session_id":"s"}))
            .unwrap();
        assert!(matches!(&parsed.ops[..], [SpanOp::Mark { name, .. }] if name == "TeammateIdle"));
    }

    #[test]
    fn post_tool_use_failure_is_error_status() {
        let parsed = ClaudeCode
            .parse(
                &json!({"hook_event_name":"PostToolUseFailure","session_id":"s",
                "tool_name":"Bash","tool_use_id":"tu_2","error":"boom"}),
            )
            .unwrap();
        assert!(matches!(
            &parsed.ops[..],
            [SpanOp::CloseTool {
                status: Status::Error(None),
                ..
            }]
        ));
    }

    #[test]
    fn missing_event_name_errors() {
        assert!(ClaudeCode.parse(&json!({"session_id":"s"})).is_err());
    }

    #[test]
    fn captures_session_source_when_present() {
        // SessionStart carries `source`; `model` is optional in current Claude.
        let parsed = ClaudeCode
            .parse(&json!({"hook_event_name":"SessionStart","session_id":"s","source":"startup"}))
            .unwrap();
        let attrs = format!("{:?}", parsed.ops);
        assert!(attrs.contains("gently.source"));
        assert!(attrs.contains("startup"));
        assert!(!attrs.contains("gently.model"));
    }

    #[test]
    fn captures_effort_and_session_end_reason() {
        // `effort` rides on tool/turn events as an object `{"level": ...}`.
        let parsed = ClaudeCode
            .parse(&json!({"hook_event_name":"PreToolUse","session_id":"s",
                "tool_name":"Bash","tool_use_id":"t1","tool_input":{},"effort":{"level":"high"}}))
            .unwrap();
        let ops = format!("{:?}", parsed.ops);
        assert!(ops.contains("gently.effort"));
        assert!(ops.contains("high"));

        // `reason` rides on SessionEnd.
        let parsed = ClaudeCode
            .parse(&json!({"hook_event_name":"SessionEnd","session_id":"s","reason":"clear"}))
            .unwrap();
        let attrs = format!("{:?}", parsed.ops);
        assert!(attrs.contains("gently.reason"));
        assert!(attrs.contains("clear"));
    }

    #[test]
    fn current_prompt_id_correlates_turn_and_parallel_tools() {
        for event in ["UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop"] {
            let parsed = ClaudeCode
                .parse(&json!({
                    "hook_event_name": event, "session_id": "s", "prompt_id": "prompt-1",
                    "tool_name": "Read", "tool_use_id": "tool-1"
                }))
                .unwrap();
            assert_eq!(parsed.turn_id.as_deref(), Some("prompt-1"), "{event}");
        }
    }

    #[test]
    fn subagent_tool_context_is_preserved_as_metadata() {
        let parsed = ClaudeCode
            .parse(&json!({
                "hook_event_name": "PreToolUse", "session_id": "s", "prompt_id": "prompt-1",
                "agent_id": "agent-1", "agent_type": "Explore", "tool_name": "Read",
                "tool_use_id": "tool-1", "tool_input": {"file_path": "synthetic.txt"}
            }))
            .unwrap();
        match &parsed.ops[..] {
            [SpanOp::OpenTool { attrs, .. }] => {
                assert!(attrs.contains(&("gently.agent_id".into(), "agent-1".into())));
                assert!(attrs.contains(&("gently.prompt_id".into(), "prompt-1".into())));
            }
            other => panic!("expected one tool open, got {other:?}"),
        }
    }

    #[test]
    fn compaction_keeps_trigger_and_digests_content() {
        for (event, field, digest_key) in [
            (
                "PreCompact",
                "custom_instructions",
                "gently.compact_instructions",
            ),
            ("PostCompact", "compact_summary", "gently.compact_summary"),
        ] {
            let mut raw = json!({"hook_event_name": event, "session_id": "s", "trigger": "manual"});
            raw[field] = json!("synthetic private compaction text");
            let parsed = ClaudeCode.parse(&raw).unwrap();
            match &parsed.ops[..] {
                [SpanOp::Mark { name, attrs }] => {
                    assert_eq!(name, event);
                    assert!(attrs.contains(&("gently.trigger".into(), "manual".into())));
                    assert!(attrs
                        .iter()
                        .any(|(k, _)| k == &format!("{digest_key}.sha256")));
                    assert!(attrs.contains(&(format!("{digest_key}.bytes"), "33".into())));
                    assert!(!format!("{parsed:?}").contains("synthetic private compaction text"));
                }
                other => panic!("compaction must remain a marker, got {other:?}"),
            }
        }
    }

    #[test]
    fn tool_batch_is_one_marker_without_reclosing_individual_tools() {
        let parsed = ClaudeCode.parse(&json!({
            "hook_event_name": "PostToolBatch", "session_id": "s", "prompt_id": "prompt-1",
            "tool_calls": [
                {"tool_name": "Read", "tool_use_id": "tool-1", "tool_response": "synthetic private text"},
                {"tool_name": "Read", "tool_use_id": "tool-2", "tool_response": [{"type":"text","text":"synthetic private text"}]}
            ]
        })).unwrap();
        match &parsed.ops[..] {
            [SpanOp::Mark { name, attrs }] => {
                assert_eq!(name, "PostToolBatch");
                assert!(attrs.contains(&("gently.tool_calls.count".into(), "2".into())));
                assert!(attrs.iter().any(|(k, _)| k == "gently.tool_calls.sha256"));
                assert!(!format!("{parsed:?}").contains("synthetic private text"));
            }
            other => panic!("batch must not duplicate tool closes, got {other:?}"),
        }
    }

    #[test]
    fn api_failure_keeps_error_type_and_hashes_freeform_details() {
        let parsed = ClaudeCode.parse(&json!({
            "hook_event_name": "StopFailure", "session_id": "s", "error": "rate_limit",
            "error_details": "synthetic private API diagnostic", "last_assistant_message": "synthetic error rendering"
        })).unwrap();
        match &parsed.ops[..] {
            [SpanOp::CloseTurn { status, attrs }] => {
                assert_eq!(*status, Status::Error(None));
                assert!(attrs.contains(&("gently.error_type".into(), "rate_limit".into())));
                assert!(attrs
                    .iter()
                    .any(|(k, _)| k == "gently.error_details.sha256"));
                assert!(!format!("{parsed:?}").contains("synthetic private API diagnostic"));
                assert!(!format!("{parsed:?}").contains("synthetic error rendering"));
            }
            other => panic!("expected failing turn close, got {other:?}"),
        }
    }

    #[test]
    fn tool_failure_hashes_output_bearing_diagnostic() {
        let parsed = ClaudeCode
            .parse(&json!({
                "hook_event_name": "PostToolUseFailure", "session_id": "s", "tool_name": "Bash",
                "tool_use_id": "tool-1", "duration_ms": 23, "is_interrupt": true,
                "error": "Exit code 1\nsynthetic private stdout and stderr"
            }))
            .unwrap();
        match &parsed.ops[..] {
            [SpanOp::CloseTool {
                status,
                duration_ms,
                attrs,
                ..
            }] => {
                assert_eq!(*status, Status::Error(None));
                assert_eq!(*duration_ms, Some(23));
                assert!(attrs.contains(&("gently.is_interrupt".into(), "true".into())));
                assert!(attrs.iter().any(|(k, _)| k == "gently.error.sha256"));
                assert!(!format!("{parsed:?}").contains("synthetic private stdout and stderr"));
            }
            other => panic!("expected failing tool close, got {other:?}"),
        }
    }

    #[test]
    fn model_switch_keeps_actual_model_names() {
        let parsed = ClaudeCode.parse(&json!({
            "hook_event_name": "PostModelSwitch", "session_id": "s", "source": "auto",
            "from_model": "claude-sonnet-4-6", "to_model": "claude-opus-4-6", "requested_model": null
        })).unwrap();
        match &parsed.ops[..] {
            [SpanOp::Mark { name, attrs }] => {
                assert_eq!(name, "PostModelSwitch");
                assert!(attrs.contains(&("gently.from_model".into(), "claude-sonnet-4-6".into())));
                assert!(attrs.contains(&("gently.to_model".into(), "claude-opus-4-6".into())));
                assert!(attrs.contains(&("gently.model".into(), "claude-opus-4-6".into())));
            }
            other => panic!("expected model-switch marker, got {other:?}"),
        }
    }

    #[test]
    fn executing_subagent_context_routes_ordinary_hooks() {
        for event in [
            "PreToolUse",
            "PostToolUse",
            "Stop",
            "StopFailure",
            "PostToolBatch",
            "PreCompact",
            "PostCompact",
        ] {
            let parsed = ClaudeCode
                .parse(&json!({
                    "hook_event_name": event, "session_id": "s", "prompt_id": "prompt-1",
                    "agent_id": "agent-1", "tool_name": "Read", "tool_use_id": "tool-1"
                }))
                .unwrap();
            assert_eq!(parsed.agent_id.as_deref(), Some("agent-1"), "{event}");
            assert_eq!(parsed.session_id, "s");
            assert_eq!(parsed.turn_id.as_deref(), Some("prompt-1"));
        }
    }

    #[test]
    fn agent_lifecycle_identifier_is_subject_not_execution_context() {
        for event in ["SubagentStart", "SubagentStop"] {
            let parsed = ClaudeCode
                .parse(&json!({
                    "hook_event_name": event, "session_id": "s", "prompt_id": "prompt-1",
                    "agent_id": "agent-1", "agent_type": "Explore"
                }))
                .unwrap();
            assert_eq!(parsed.agent_id, None, "{event} reports the subject agent");
            match &parsed.ops[..] {
                [SpanOp::OpenAgent { agent_id, .. }] | [SpanOp::CloseAgent { agent_id, .. }] => {
                    assert_eq!(agent_id, "agent-1")
                }
                other => panic!("expected agent lifecycle, got {other:?}"),
            }
        }
    }

    #[test]
    fn legacy_payloads_and_missing_lifecycle_ids_remain_tolerated() {
        for prompt in [serde_json::Value::Null, json!(""), json!(123)] {
            let parsed = ClaudeCode
                .parse(&json!({
                    "hook_event_name": "PreToolUse", "session_id": "s", "prompt_id": prompt,
                    "tool_name": "Read"
                }))
                .unwrap();
            assert_eq!(parsed.turn_id, None);
            assert_eq!(parsed.agent_id, None);
            assert!(matches!(
                &parsed.ops[..],
                [SpanOp::OpenTool {
                    tool_use_id: None,
                    ..
                }]
            ));
        }
        for event in ["SubagentStart", "SubagentStop"] {
            let parsed = ClaudeCode
                .parse(&json!({
                    "hook_event_name": event, "session_id": "s", "prompt_id": "prompt-1"
                }))
                .unwrap();
            assert_eq!(parsed.turn_id.as_deref(), Some("prompt-1"));
            assert!(matches!(&parsed.ops[..], [SpanOp::Mark {name, ..}] if name == event));
        }
    }

    #[test]
    fn unknown_api_error_is_hashed_without_raw_category() {
        let parsed = ClaudeCode.parse(&json!({
            "hook_event_name": "StopFailure", "session_id": "s",
            "error": "synthetic future diagnostic containing private output", "error_details": null
        })).unwrap();
        match &parsed.ops[..] {
            [SpanOp::CloseTurn { status, attrs }] => {
                assert_eq!(*status, Status::Error(None));
                assert!(attrs.iter().any(|(key, _)| key == "gently.error.sha256"));
                assert!(!attrs.iter().any(|(key, _)| key == "gently.error_type"
                    || key.starts_with("gently.error_details.")));
                assert!(!format!("{parsed:?}").contains("synthetic future diagnostic"));
            }
            other => panic!("expected API failure close, got {other:?}"),
        }
    }

    #[test]
    fn automatic_compaction_null_instructions_have_no_content_digest() {
        let parsed = ClaudeCode.parse(&json!({
            "hook_event_name": "PreCompact", "session_id": "s", "trigger": "auto", "custom_instructions": null
        })).unwrap();
        match &parsed.ops[..] {
            [SpanOp::Mark { attrs, .. }] => {
                assert!(attrs.contains(&("gently.trigger".into(), "auto".into())));
                assert!(!attrs
                    .iter()
                    .any(|(key, _)| key.starts_with("gently.compact_instructions.")));
            }
            other => panic!("expected compaction marker, got {other:?}"),
        }
    }

    #[test]
    fn empty_lifecycle_agent_ids_fall_back_to_markers() {
        for event in ["SubagentStart", "SubagentStop"] {
            let parsed = ClaudeCode
                .parse(&json!({
                    "hook_event_name": event, "session_id": "s", "agent_id": ""
                }))
                .unwrap();
            assert_eq!(parsed.agent_id, None);
            assert!(matches!(&parsed.ops[..], [SpanOp::Mark {name, ..}] if name == event));
        }
    }

    #[test]
    fn compaction_trigger_accepts_only_documented_categories() {
        let parsed = ClaudeCode
            .parse(&json!({
                "hook_event_name": "PostCompact", "session_id": "s",
                "trigger": "synthetic unexpected private trigger text"
            }))
            .unwrap();
        match &parsed.ops[..] {
            [SpanOp::Mark { attrs, .. }] => {
                assert!(!attrs.iter().any(|(key, _)| key == "gently.trigger"));
                assert!(
                    !format!("{parsed:?}").contains("synthetic unexpected private trigger text")
                );
            }
            other => panic!("expected compaction marker, got {other:?}"),
        }
    }
}
