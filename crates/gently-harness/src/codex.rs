//! Codex CLI hook adapter.
//!
//! Codex 0.135.0 exposes a Claude-Code-style hooks system (stdin JSON, the same
//! payload field names) - a superset of Claude's events. We model the lifecycle
//! events that map onto spans and let everything else (`PermissionRequest`,
//! `PreCompact`, `PostCompact`, …) fall through to [`SpanOp::Mark`] via the
//! unknown-event default, keeping the adapter forward-compatible. Codex has no
//! `SessionEnd`, `StopFailure`, or `PostToolUseFailure` hook, so the session root
//! is never explicitly closed and turn/tool status is always `Ok` (documented in
//! the design spec). No raw prompt or tool content is placed in a span - only a
//! digest and byte length.

use crate::hooks::{common_attrs, mark, push_digest, push_value_digest, str_field};
use crate::{Harness, HarnessError, Parsed, SpanOp};
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
        let turn_id = str_field(raw, "turn_id");

        let ops = match event {
            "SessionStart" => vec![SpanOp::OpenSession {
                attrs: common_attrs(raw, event),
            }],
            "UserPromptSubmit" => {
                let mut attrs = common_attrs(raw, event);
                if let Some(p) = str_field(raw, "prompt") {
                    push_digest(&mut attrs, "gently.prompt", p.as_bytes());
                }
                vec![SpanOp::OpenTurn { attrs }]
            }
            // Codex has no StopFailure hook; a turn always closes Ok. Aborts are
            // only visible in the rollout transcript, not via hooks.
            "Stop" => vec![SpanOp::CloseTurn {
                status: Status::Ok,
                attrs: common_attrs(raw, event),
            }],
            "PreToolUse" => {
                let tool_name = str_field(raw, "tool_name").unwrap_or_default();
                let mut attrs = common_attrs(raw, event);
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
            // Codex has no PostToolUseFailure hook and no universal success flag
            // on the payload, so tool status is always Ok at MVP.
            "PostToolUse" => {
                let tool_name = str_field(raw, "tool_name").unwrap_or_default();
                let mut attrs = common_attrs(raw, event);
                attrs.push(("gently.tool_name".into(), tool_name.clone()));
                if let Some(resp) = raw.get("tool_response") {
                    push_value_digest(&mut attrs, "gently.tool_response", resp);
                }
                vec![SpanOp::CloseTool {
                    tool_use_id: str_field(raw, "tool_use_id"),
                    tool_name,
                    status: Status::Ok,
                    duration_ms: None,
                    attrs,
                }]
            }
            "SubagentStart" => {
                let Some(agent_id) = str_field(raw, "agent_id") else {
                    return Ok(Parsed {
                        session_id,
                        cwd,
                        transcript_path,
                        turn_id,
                        ops: vec![mark(raw, event)],
                    });
                };
                vec![SpanOp::OpenAgent {
                    agent_id,
                    parent_tool_use_id: str_field(raw, "tool_use_id"),
                    attrs: common_attrs(raw, event),
                }]
            }
            "SubagentStop" => {
                let Some(agent_id) = str_field(raw, "agent_id") else {
                    return Ok(Parsed {
                        session_id,
                        cwd,
                        transcript_path,
                        turn_id,
                        ops: vec![mark(raw, event)],
                    });
                };
                vec![SpanOp::CloseAgent {
                    agent_id,
                    status: Status::Ok,
                    attrs: common_attrs(raw, event),
                }]
            }
            _ => vec![mark(raw, event)],
        };

        Ok(Parsed {
            session_id,
            cwd,
            transcript_path,
            turn_id,
            ops,
        })
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
        // raw tool_input content is digested, never placed verbatim in a span
        let pre_attrs = format!("{:?}", parsed.ops);
        assert!(pre_attrs.contains("gently.tool_input.sha256"));
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
                assert_eq!(*status, Status::Ok);
            }
            other => panic!("expected CloseTool, got {other:?}"),
        }
    }

    #[test]
    fn user_prompt_opens_turn_with_digest_not_content() {
        let parsed = Codex
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
}
