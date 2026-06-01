//! Claude Code hook adapter.
//!
//! Maps the documented Claude Code hook events onto [`SpanOp`]s. The mapping is
//! deliberately tolerant: only `session_id` and `hook_event_name` are required;
//! any of the 30 events we do not model becomes a [`SpanOp::Mark`], so the
//! adapter is forward-compatible by construction. No raw prompt or tool content
//! is ever placed in a span - only a digest and byte length.

use crate::hooks::{common_attrs, mark, push_digest, push_value_digest, str_field, u64_field};
use crate::{Harness, HarnessError, Parsed, SpanOp};
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

        let ops = match event {
            "SessionStart" => vec![SpanOp::OpenSession {
                attrs: common_attrs(raw, event),
            }],
            "SessionEnd" => vec![SpanOp::CloseSession {
                status: Status::Ok,
                attrs: common_attrs(raw, event),
            }],
            "UserPromptSubmit" => {
                let mut attrs = common_attrs(raw, event);
                if let Some(p) = str_field(raw, "prompt") {
                    push_digest(&mut attrs, "gently.prompt", p.as_bytes());
                }
                vec![SpanOp::OpenTurn { attrs }]
            }
            "Stop" => vec![SpanOp::CloseTurn {
                status: Status::Ok,
                attrs: common_attrs(raw, event),
            }],
            "StopFailure" => vec![SpanOp::CloseTurn {
                status: Status::Error(None),
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
            "PostToolUse" | "PostToolUseFailure" => {
                let tool_name = str_field(raw, "tool_name").unwrap_or_default();
                let mut attrs = common_attrs(raw, event);
                attrs.push(("gently.tool_name".into(), tool_name.clone()));
                if let Some(resp) = raw.get("tool_response") {
                    push_value_digest(&mut attrs, "gently.tool_response", resp);
                }
                let status = if event == "PostToolUseFailure" {
                    Status::Error(str_field(raw, "error"))
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
                let Some(agent_id) = str_field(raw, "agent_id") else {
                    return Ok(Parsed {
                        session_id,
                        cwd,
                        transcript_path,
                        turn_id: None,
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
                        turn_id: None,
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
            turn_id: None,
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
        assert!(matches!(&parsed.ops[..],
            [SpanOp::CloseTool { status: Status::Error(Some(e)), .. }] if e == "boom"));
    }

    #[test]
    fn missing_event_name_errors() {
        assert!(ClaudeCode.parse(&json!({"session_id":"s"})).is_err());
    }
}
