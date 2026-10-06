//! Codex CLI hook adapter, checked against Codex 0.160.0.
//!
//! The command-input contract is documented at <https://learn.chatgpt.com/docs/hooks>
//! and versioned in `openai/codex`, tag `rust-v0.160.0`, under
//! `codex-rs/hooks/src/schema.rs`. SessionEnd closes the session; Interrupt
//! closes the active main-agent turn without treating cancellation as failure.
//! Compaction hooks remain markers within their current turn.
//!
//! Normal turn/tool hooks inside a subagent keep the root session_id and carry
//! the current agent_id. SubagentStart/Stop instead identify the child whose
//! lifecycle is changing. Keep those two identities separate for parenting.
//!
//! Codex has no universal tool-success field or failure hook. In this release,
//! unified exec reports only output text through PostToolUse, even for nonzero
//! exits (`codex-rs/core/src/tools/context.rs`). MCP results carry their typed
//! isError flag. Classify those structured results; leave opaque status Unset.
//! Prompt and tool content is recorded only as a byte length.

use crate::hooks::{
    common_attrs, push_first_str_length, push_observed_tool_attrs, push_value_length, str_field,
};
use crate::{Attrs, Harness, HarnessError, Parsed, SpanOp};
use gently_core::Status;

/// The Codex CLI harness adapter.
pub struct Codex;

impl Harness for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn parse(&self, raw: &serde_json::Value) -> Result<Parsed, HarnessError> {
        let event_owned = str_field(raw, "hook_event_name")
            .ok_or(HarnessError::MissingField("hook_event_name"))?;
        let event = event_owned.as_str();
        let session_id =
            str_field(raw, "session_id").ok_or(HarnessError::MissingField("session_id"))?;
        let cwd = str_field(raw, "cwd").unwrap_or_default();
        let transcript_path = str_field(raw, "transcript_path");
        let turn_id = str_field(raw, "turn_id").filter(|id| !id.is_empty());
        // These schemas carry the execution context of a normal hook running
        // inside a subagent. Lifecycle hooks carry a child subject instead;
        // Interrupt is main-agent-only in Codex 0.160.0.
        let current_agent_id = if matches!(
            event,
            "UserPromptSubmit"
                | "PreToolUse"
                | "PermissionRequest"
                | "PostToolUse"
                | "Stop"
                | "PreCompact"
                | "PostCompact"
        ) {
            str_field(raw, "agent_id").filter(|id| !id.is_empty())
        } else {
            None
        };

        let ops = match event {
            "SessionStart" => vec![SpanOp::OpenSession {
                attrs: codex_attrs(raw, event),
            }],
            "SessionEnd" => vec![SpanOp::CloseSession {
                status: Status::Ok,
                attrs: codex_attrs(raw, event),
            }],
            "UserPromptSubmit" => {
                let mut attrs = codex_attrs(raw, event);
                push_first_str_length(
                    &mut attrs,
                    "gently.prompt",
                    raw,
                    &["prompt", "user_prompt", "user"],
                );
                vec![SpanOp::OpenTurn { attrs }]
            }
            "Stop" => vec![SpanOp::CloseTurn {
                status: Status::Ok,
                attrs: codex_attrs(raw, event),
            }],
            "Interrupt" => {
                let mut attrs = codex_attrs(raw, event);
                attrs.push(("gently.interrupted".into(), "true".into()));
                vec![SpanOp::CloseTurn {
                    status: Status::Unset,
                    attrs,
                }]
            }
            "PermissionRequest" => {
                let mut attrs = codex_attrs(raw, event);
                push_observed_tool_attrs(&mut attrs, raw);
                vec![SpanOp::Mark {
                    name: event.into(),
                    attrs,
                }]
            }
            "PreToolUse" => {
                let tool_name = str_field(raw, "tool_name").unwrap_or_default();
                let attrs = tool_attrs(raw, event, &tool_name);
                vec![SpanOp::OpenTool {
                    tool_use_id: str_field(raw, "tool_use_id").filter(|id| !id.is_empty()),
                    tool_name,
                    attrs,
                }]
            }
            "PostToolUse" => {
                let tool_name = str_field(raw, "tool_name").unwrap_or_default();
                let mut attrs = tool_attrs(raw, event, &tool_name);
                let status = tool_status(&tool_name, raw.get("tool_response"));
                if let Some(resp) = raw.get("tool_response") {
                    push_value_length(&mut attrs, "gently.tool_response", resp);
                }
                vec![SpanOp::CloseTool {
                    tool_use_id: str_field(raw, "tool_use_id").filter(|id| !id.is_empty()),
                    tool_name,
                    status,
                    duration_ms: None,
                    attrs,
                }]
            }
            "SubagentStart" => {
                let Some(agent_id) = str_field(raw, "agent_id").filter(|id| !id.is_empty()) else {
                    return Ok(Parsed {
                        session_id,
                        cwd,
                        transcript_path,
                        turn_id,
                        agent_id: current_agent_id,
                        ops: vec![codex_mark(raw, event)],
                    });
                };
                vec![SpanOp::OpenAgent {
                    agent_id,
                    parent_tool_use_id: str_field(raw, "tool_use_id").filter(|id| !id.is_empty()),
                    attrs: codex_attrs(raw, event),
                }]
            }
            "SubagentStop" => {
                let Some(agent_id) = str_field(raw, "agent_id").filter(|id| !id.is_empty()) else {
                    return Ok(Parsed {
                        session_id,
                        cwd,
                        transcript_path,
                        turn_id,
                        agent_id: current_agent_id,
                        ops: vec![codex_mark(raw, event)],
                    });
                };
                vec![SpanOp::CloseAgent {
                    agent_id,
                    status: Status::Ok,
                    attrs: codex_attrs(raw, event),
                }]
            }
            _ => vec![codex_mark(raw, event)],
        };

        Ok(Parsed {
            session_id,
            cwd,
            transcript_path,
            turn_id,
            agent_id: current_agent_id,
            ops,
        })
    }
}

/// Codex-only metadata with bounded values; arbitrary content stays out of attrs.
fn codex_attrs(raw: &serde_json::Value, event: &str) -> Attrs {
    let mut attrs = common_attrs(raw, event);
    if matches!(event, "Stop" | "SubagentStop") {
        if let Some(active) = raw
            .get("stop_hook_active")
            .and_then(serde_json::Value::as_bool)
        {
            attrs.push(("gently.stop_hook_active".into(), active.to_string()));
        }
    }
    if matches!(event, "PreCompact" | "PostCompact") {
        if let Some(trigger @ ("manual" | "auto")) =
            raw.get("trigger").and_then(serde_json::Value::as_str)
        {
            attrs.push(("gently.trigger".into(), trigger.into()));
        }
    }
    attrs
}

fn codex_mark(raw: &serde_json::Value, event: &str) -> SpanOp {
    SpanOp::Mark {
        name: event.into(),
        attrs: codex_attrs(raw, event),
    }
}

fn tool_attrs(raw: &serde_json::Value, event: &str, tool_name: &str) -> Attrs {
    let mut attrs = codex_attrs(raw, event);
    attrs.push(("gently.tool_name".into(), tool_name.into()));
    if let Some(input) = raw.get("tool_input") {
        push_value_length(&mut attrs, "gently.tool_input", input);
    }
    attrs
}

fn tool_status(tool_name: &str, response: Option<&serde_json::Value>) -> Status {
    // Codex forwards MCP CallToolResult, whose required content array and
    // optional boolean isError are distinct from arbitrary function output.
    // Do not parse shell output, infer status from prose, or trust similarly
    // named keys in a non-MCP tool result.
    if !tool_name.starts_with("mcp__") {
        return Status::Unset;
    }
    let Some(result) = response.and_then(serde_json::Value::as_object) else {
        return Status::Unset;
    };
    if !result
        .get("content")
        .is_some_and(serde_json::Value::is_array)
    {
        return Status::Unset;
    }
    match result.get("isError") {
        Some(serde_json::Value::Bool(true)) => Status::Error(None),
        Some(serde_json::Value::Bool(false)) | None => Status::Ok,
        _ => Status::Unset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn pre_then_post_tool_use_pairs_via_tool_use_id() {
        let h = Codex;
        let pre = json!({"hook_event_name":"PreToolUse","session_id":"s",
            "cwd":"/w","tool_name":"shell_command","tool_use_id":"call_1",
            "tool_input":{"command":"ls"}});
        let parsed = h.parse(&pre).unwrap();
        assert_eq!(parsed.session_id, "s");
        match &parsed.ops[..] {
            [SpanOp::OpenTool {
                tool_use_id,
                tool_name,
                ..
            }] => {
                assert_eq!(tool_use_id.as_deref(), Some("call_1"));
                assert_eq!(tool_name, "shell_command");
            }
            other => panic!("expected OpenTool, got {other:?}"),
        }
        // raw tool_input content is measured, never placed verbatim in a span
        let pre_attrs = format!("{:?}", parsed.ops);
        assert!(pre_attrs.contains("gently.tool_input.bytes"));
        assert!(!pre_attrs.contains("\"ls\""));

        let post = json!({"hook_event_name":"PostToolUse","session_id":"s",
            "cwd":"/w","tool_name":"shell_command","tool_use_id":"call_1",
            "tool_response":{"output":"a"}});
        let parsed = h.parse(&post).unwrap();
        match &parsed.ops[..] {
            [SpanOp::CloseTool {
                tool_use_id,
                status,
                ..
            }] => {
                assert_eq!(tool_use_id.as_deref(), Some("call_1"));
                assert_eq!(*status, Status::Unset);
            }
            other => panic!("expected CloseTool, got {other:?}"),
        }
    }

    #[test]
    fn user_prompt_opens_turn_with_length_not_content() {
        let parsed = Codex
            .parse(
                &json!({"hook_event_name":"UserPromptSubmit","session_id":"s",
                "prompt":"secret content here"}),
            )
            .unwrap();
        match &parsed.ops[..] {
            [SpanOp::OpenTurn { attrs }] => {
                let joined = format!("{attrs:?}");
                assert!(joined.contains("gently.prompt.bytes"));
                assert!(joined.contains("gently.prompt.bytes"));
                assert!(!joined.contains("secret content"));
            }
            other => panic!("expected OpenTurn, got {other:?}"),
        }
    }

    #[test]
    fn assistant_message_gets_length_not_content() {
        let parsed = Codex
            .parse(&json!({"hook_event_name":"Stop","session_id":"s",
                "last_assistant_message":"assistant secret here"}))
            .unwrap();
        match &parsed.ops[..] {
            [SpanOp::CloseTurn { attrs, .. }] => {
                let joined = format!("{attrs:?}");
                assert!(joined.contains("gently.assistant.bytes"));
                assert!(joined.contains("gently.assistant.bytes"));
                assert!(!joined.contains("assistant secret"));
            }
            other => panic!("expected CloseTurn, got {other:?}"),
        }
    }

    #[test]
    fn session_start_then_stop_open_and_close_turn() {
        let h = Codex;
        let start = h
            .parse(&json!({"hook_event_name":"SessionStart","session_id":"s","cwd":"/w"}))
            .unwrap();
        assert!(matches!(&start.ops[..], [SpanOp::OpenSession { .. }]));

        let stop = h
            .parse(&json!({"hook_event_name":"Stop","session_id":"s"}))
            .unwrap();
        assert!(matches!(
            &stop.ops[..],
            [SpanOp::CloseTurn {
                status: Status::Ok,
                ..
            }]
        ));
    }

    #[test]
    fn subagent_start_opens_agent_when_id_present() {
        let parsed = Codex
            .parse(&json!({"hook_event_name":"SubagentStart","session_id":"s",
                "agent_id":"ag1","tool_use_id":"call_2","agent_type":"explore"}))
            .unwrap();
        match &parsed.ops[..] {
            [SpanOp::OpenAgent {
                agent_id,
                parent_tool_use_id,
                ..
            }] => {
                assert_eq!(agent_id, "ag1");
                assert_eq!(parent_tool_use_id.as_deref(), Some("call_2"));
            }
            other => panic!("expected OpenAgent, got {other:?}"),
        }
    }

    #[test]
    fn subagent_without_id_falls_back_to_mark() {
        let parsed = Codex
            .parse(&json!({"hook_event_name":"SubagentStart","session_id":"s"}))
            .unwrap();
        assert!(matches!(&parsed.ops[..], [SpanOp::Mark { name, .. }] if name == "SubagentStart"));
    }

    #[test]
    fn permission_request_becomes_mark() {
        let parsed = Codex
            .parse(&json!({"hook_event_name":"PermissionRequest","session_id":"s"}))
            .unwrap();
        assert!(
            matches!(&parsed.ops[..], [SpanOp::Mark { name, .. }] if name == "PermissionRequest")
        );
    }

    #[test]
    fn subagent_stop_closes_agent_when_id_present() {
        let parsed = Codex
            .parse(&json!({"hook_event_name":"SubagentStop","session_id":"s","agent_id":"ag1"}))
            .unwrap();
        match &parsed.ops[..] {
            [SpanOp::CloseAgent {
                agent_id, status, ..
            }] => {
                assert_eq!(agent_id, "ag1");
                assert_eq!(*status, Status::Ok);
            }
            other => panic!("expected CloseAgent, got {other:?}"),
        }
    }

    #[test]
    fn subagent_stop_without_id_falls_back_to_mark() {
        let parsed = Codex
            .parse(&json!({"hook_event_name":"SubagentStop","session_id":"s"}))
            .unwrap();
        assert!(matches!(&parsed.ops[..], [SpanOp::Mark { name, .. }] if name == "SubagentStop"));
    }

    #[test]
    fn missing_event_name_errors() {
        assert!(Codex.parse(&json!({"session_id":"s"})).is_err());
    }

    #[test]
    fn captures_model_and_source_from_payload() {
        // SessionStart carries both `model` and `source`.
        let parsed = Codex
            .parse(&json!({"hook_event_name":"SessionStart","session_id":"s",
                "model":"gpt-5.5","source":"resume"}))
            .unwrap();
        let attrs = format!("{:?}", parsed.ops);
        assert!(attrs.contains("gently.model"));
        assert!(attrs.contains("gpt-5.5"));
        assert!(attrs.contains("gently.source"));
        assert!(attrs.contains("resume"));

        // A turn-scoped event carries `model` but no `source`.
        let parsed = Codex
            .parse(
                &json!({"hook_event_name":"UserPromptSubmit","session_id":"s",
                "turn_id":"t1","model":"gpt-5.5-codex"}),
            )
            .unwrap();
        let attrs = format!("{:?}", parsed.ops);
        assert!(attrs.contains("gently.model"));
        assert!(attrs.contains("gpt-5.5-codex"));
        assert!(!attrs.contains("gently.source"));
    }

    #[test]
    fn captures_agent_transcript_path_on_subagent_stop() {
        let parsed = Codex
            .parse(&json!({"hook_event_name":"SubagentStop","session_id":"s",
                "agent_id":"ag1","agent_transcript_path":"/tmp/ag1.jsonl"}))
            .unwrap();
        let attrs = format!("{:?}", parsed.ops);
        assert!(attrs.contains("gently.agent_transcript_path"));
        assert!(attrs.contains("/tmp/ag1.jsonl"));
    }

    // Synthetic payloads matching the 0.160.0 command-input schemas; never
    // captured from a user's prompt, rollout, environment, or authentication.
    #[test]
    fn session_end_closes_session_with_nullable_transcript() {
        let parsed = Codex
            .parse(&json!({
                "hook_event_name": "SessionEnd", "session_id": "s", "cwd": "/w",
                "transcript_path": null, "reason": "other"
            }))
            .unwrap();
        assert_eq!(parsed.transcript_path, None);
        assert!(matches!(
            &parsed.ops[..],
            [SpanOp::CloseSession {
                status: Status::Ok,
                ..
            }]
        ));
    }

    #[test]
    fn interrupt_closes_exact_turn_without_recording_a_failure() {
        let parsed = Codex
            .parse(&json!({
                "hook_event_name": "Interrupt", "session_id": "s", "cwd": "/w",
                "turn_id": "interrupted-turn", "permission_mode": "default"
            }))
            .unwrap();
        assert_eq!(parsed.turn_id.as_deref(), Some("interrupted-turn"));
        match &parsed.ops[..] {
            [SpanOp::CloseTurn { status, attrs }] => {
                assert_eq!(*status, Status::Unset);
                assert!(attrs.contains(&("gently.interrupted".into(), "true".into())));
            }
            other => panic!("expected interrupted CloseTurn, got {other:?}"),
        }
    }

    #[test]
    fn compaction_marks_keep_trigger_and_active_turn() {
        for event in ["PreCompact", "PostCompact"] {
            for trigger in ["manual", "auto"] {
                let parsed = Codex
                    .parse(&json!({
                        "hook_event_name": event, "session_id": "s", "turn_id": "t",
                        "transcript_path": null, "model": "gpt-test", "trigger": trigger
                    }))
                    .unwrap();
                assert_eq!(parsed.turn_id.as_deref(), Some("t"));
                match &parsed.ops[..] {
                    [SpanOp::Mark { name, attrs }] => {
                        assert_eq!(name, event);
                        assert!(attrs.contains(&("gently.trigger".into(), trigger.into())));
                    }
                    other => panic!("expected compaction Mark, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn compaction_does_not_record_unrecognized_trigger_content() {
        let parsed = Codex
            .parse(&json!({
                "hook_event_name": "PreCompact", "session_id": "s",
                "trigger": "private arbitrary text"
            }))
            .unwrap();
        assert!(!format!("{:?}", parsed.ops).contains("private arbitrary text"));
    }

    #[test]
    fn stop_retains_hook_reentry_flag() {
        for event in ["Stop", "SubagentStop"] {
            let parsed = Codex
                .parse(&json!({
                    "hook_event_name": event, "session_id": "s", "agent_id": "agent",
                    "stop_hook_active": true, "last_assistant_message": null
                }))
                .unwrap();
            let attrs = match &parsed.ops[..] {
                [SpanOp::CloseTurn { attrs, .. }] | [SpanOp::CloseAgent { attrs, .. }] => attrs,
                other => panic!("expected lifecycle close, got {other:?}"),
            };
            assert!(attrs.contains(&("gently.stop_hook_active".into(), "true".into())));
            assert!(!attrs
                .iter()
                .any(|(key, _)| key.starts_with("gently.assistant.")));
        }
    }

    #[test]
    fn mcp_structured_result_classifies_failure_without_leaking_content() {
        for (response, expected) in [
            (
                json!({"content": [{"type": "text", "text": "private tool output"}], "isError": true}),
                Status::Error(None),
            ),
            (json!({"content": [], "isError": false}), Status::Ok),
            (json!({"content": []}), Status::Ok),
            (json!({"content": [], "isError": "false"}), Status::Unset),
        ] {
            let parsed = Codex
                .parse(&json!({
                    "hook_event_name": "PostToolUse", "session_id": "s", "turn_id": "t",
                    "tool_name": "mcp__example__read", "tool_use_id": "call-mcp",
                    "tool_input": {"query": "private tool input"}, "tool_response": response
                }))
                .unwrap();
            match &parsed.ops[..] {
                [SpanOp::CloseTool {
                    status,
                    tool_use_id,
                    attrs,
                    ..
                }] => {
                    assert_eq!(*status, expected);
                    assert_eq!(tool_use_id.as_deref(), Some("call-mcp"));
                    let attrs = format!("{attrs:?}");
                    assert!(attrs.contains("gently.tool_response.bytes"));
                    assert!(attrs.contains("gently.tool_input.bytes"));
                    assert!(!attrs.contains("private tool output"));
                    assert!(!attrs.contains("private tool input"));
                }
                other => panic!("expected CloseTool, got {other:?}"),
            }
        }
    }

    #[test]
    fn opaque_tool_result_does_not_infer_success_or_failure() {
        // Unified exec emits only output text, including for a nonzero exit.
        // A line that resembles an exit header can also be user-produced text.
        for (tool_name, response) in [
            (
                "Bash",
                json!("Process exited with code 9\nprivate tool output"),
            ),
            ("Bash", json!({"exit_code": 9, "success": false})),
            (
                "apply_patch",
                json!("Success. Updated the following files:\nM file"),
            ),
            (
                "mcp__example__read",
                json!("private output without a CallToolResult"),
            ),
            ("custom", json!({"content": [], "isError": true})),
        ] {
            let parsed = Codex
                .parse(&json!({
                    "hook_event_name": "PostToolUse", "session_id": "s",
                    "tool_name": tool_name, "tool_use_id": "call-opaque", "tool_response": response
                }))
                .unwrap();
            assert!(matches!(
                &parsed.ops[..],
                [SpanOp::CloseTool {
                    status: Status::Unset,
                    ..
                }]
            ));
            assert!(!format!("{:?}", parsed.ops).contains("private tool output"));
        }
    }

    #[test]
    fn permission_request_records_only_tool_metadata_and_input_length() {
        let parsed = Codex
            .parse(&json!({
                "hook_event_name": "PermissionRequest", "session_id": "s", "turn_id": "t",
                "tool_name": "Bash", "tool_input": {"command": "private command"}
            }))
            .unwrap();
        match &parsed.ops[..] {
            [SpanOp::Mark { name, attrs }] => {
                assert_eq!(name, "PermissionRequest");
                assert!(attrs.contains(&("gently.hook.tool_name".into(), "Bash".into())));
                let attrs = format!("{attrs:?}");
                assert!(attrs.contains("gently.tool_input.bytes"));
                assert!(!attrs.contains("private command"));
            }
            other => panic!("expected permission Mark, got {other:?}"),
        }
    }

    #[test]
    fn subagent_execution_hooks_preserve_root_session_and_current_agent() {
        for event in [
            "UserPromptSubmit",
            "PreToolUse",
            "PermissionRequest",
            "PostToolUse",
            "Stop",
            "PreCompact",
            "PostCompact",
        ] {
            let parsed = Codex
                .parse(&json!({
                    "hook_event_name": event, "session_id": "root-session", "turn_id": "child-turn",
                    "agent_id": "child-agent", "agent_type": "explorer", "transcript_path": null,
                    "tool_name": "Bash", "tool_use_id": "child-call", "trigger": "auto"
                }))
                .unwrap();
            assert_eq!(parsed.session_id, "root-session");
            assert_eq!(parsed.agent_id.as_deref(), Some("child-agent"), "{event}");
            assert_eq!(parsed.turn_id.as_deref(), Some("child-turn"));
        }
    }

    #[test]
    fn lifecycle_agent_id_is_child_subject_not_execution_context() {
        for event in [
            "SubagentStart",
            "SubagentStop",
            "SessionStart",
            "SessionEnd",
            "Interrupt",
        ] {
            let parsed = Codex.parse(&json!({
                "hook_event_name": event, "session_id": "root-session", "agent_id": "child-agent"
            })).unwrap();
            assert_eq!(parsed.agent_id, None, "{event}");
        }
        let root = Codex
            .parse(&json!({
                "hook_event_name": "Stop", "session_id": "root-session", "agent_id": null
            }))
            .unwrap();
        assert_eq!(root.agent_id, None);
    }

    #[test]
    fn empty_turn_and_current_agent_ids_keep_legacy_root_context() {
        let parsed = Codex
            .parse(&json!({
                "hook_event_name": "PreToolUse", "session_id": "root-session",
                "turn_id": "", "agent_id": "", "tool_name": "Bash", "tool_use_id": "call"
            }))
            .unwrap();
        assert_eq!(parsed.turn_id, None);
        assert_eq!(parsed.agent_id, None);
        assert!(matches!(&parsed.ops[..], [SpanOp::OpenTool { .. }]));
    }

    #[test]
    fn empty_lifecycle_agent_id_falls_back_to_marker() {
        for event in ["SubagentStart", "SubagentStop"] {
            let parsed = Codex
                .parse(&json!({
                    "hook_event_name": event, "session_id": "root-session", "agent_id": ""
                }))
                .unwrap();
            assert_eq!(parsed.agent_id, None);
            assert!(matches!(&parsed.ops[..], [SpanOp::Mark { name, .. }] if name == event));
        }
    }
}
