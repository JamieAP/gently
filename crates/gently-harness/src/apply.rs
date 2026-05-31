//! Apply parsed [`SpanOp`]s against the local store, producing completed spans.
//!
//! This is the only stateful part of the harness layer. It owns turn-counter
//! resolution, parent linking, deterministic id derivation, and timestamps, so
//! the per-harness parsers stay pure. Every emitted [`Span`] is self-contained:
//! it carries its own deterministic `parent_span_id` whether or not the parent
//! span was ever opened, so a trace renders even if `SessionStart`/`UserPromptSubmit`
//! never fired or the local state was wiped.

use crate::{Attrs, Parsed, SpanOp};
use gently_core::{Span, SpanId, SpanKind, Status, TraceId};
use gently_store::{OpenSpan, Store};

/// Apply one event's operations. Opens are persisted; closes/marks return the
/// completed spans the caller should encode and enqueue. `now_nanos` is the
/// wall-clock at hook time (passed in to keep this testable).
pub fn apply(
    store: &Store,
    parsed: &Parsed,
    now_nanos: u64,
) -> Result<Vec<Span>, gently_store::StoreError> {
    let session = &parsed.session_id;
    let trace_id = TraceId::from_session(session);
    let mut emitted = Vec::new();

    for op in &parsed.ops {
        match op {
            SpanOp::OpenSession { attrs } => {
                store.open_span(&open(
                    "session",
                    None,
                    "session",
                    SpanKind::Internal,
                    now_nanos,
                    attrs,
                    session,
                ))?;
                // Emit a provisional session-root span immediately so the root
                // id always exists, even if SessionEnd never fires (crash/kill).
                // CloseSession re-emits it with the full duration; the span id is
                // deterministic, so the idempotent ingest just overwrites this.
                emitted.push(Span {
                    trace_id,
                    span_id: SpanId::derive(session, "session"),
                    parent_span_id: None,
                    name: "session".into(),
                    kind: SpanKind::Internal,
                    start_unix_nano: now_nanos,
                    end_unix_nano: now_nanos,
                    status: Status::Unset,
                    attributes: attrs.clone(),
                });
            }
            SpanOp::CloseSession { status, attrs } => {
                emitted.push(close(
                    store,
                    session,
                    trace_id,
                    "session",
                    "session",
                    SpanKind::Internal,
                    now_nanos,
                    status,
                    attrs,
                    None,
                )?);
            }
            SpanOp::OpenTurn { attrs } => {
                let n = store.next_turn_index(session)?;
                let key = turn_key(n);
                let parent = SpanId::derive(session, "session");
                store.open_span(&open(
                    &key,
                    Some(parent),
                    &key,
                    SpanKind::Internal,
                    now_nanos,
                    attrs,
                    session,
                ))?;
            }
            SpanOp::CloseTurn { status, attrs } => {
                let key = turn_key(store.current_turn(session)?);
                emitted.push(close(
                    store,
                    session,
                    trace_id,
                    &key,
                    &key,
                    SpanKind::Internal,
                    now_nanos,
                    status,
                    attrs,
                    None,
                )?);
            }
            SpanOp::OpenTool {
                tool_use_id,
                tool_name,
                attrs,
            } => {
                let key = tool_key(tool_use_id.as_deref(), tool_name);
                let parent = SpanId::derive(session, &turn_key(store.current_turn(session)?));
                store.open_span(&open(
                    &key,
                    Some(parent),
                    tool_name,
                    SpanKind::Client,
                    now_nanos,
                    attrs,
                    session,
                ))?;
            }
            SpanOp::CloseTool {
                tool_use_id,
                tool_name,
                status,
                duration_ms,
                attrs,
            } => {
                let key = tool_key(tool_use_id.as_deref(), tool_name);
                emitted.push(close(
                    store,
                    session,
                    trace_id,
                    &key,
                    tool_name,
                    SpanKind::Client,
                    now_nanos,
                    status,
                    attrs,
                    *duration_ms,
                )?);
            }
            SpanOp::OpenAgent {
                agent_id,
                parent_tool_use_id,
                attrs,
            } => {
                let key = format!("agent:{agent_id}");
                let parent = match parent_tool_use_id {
                    Some(tu) => SpanId::derive(session, &format!("tool:{tu}")),
                    None => SpanId::derive(session, &turn_key(store.current_turn(session)?)),
                };
                store.open_span(&open(
                    &key,
                    Some(parent),
                    &key,
                    SpanKind::Internal,
                    now_nanos,
                    attrs,
                    session,
                ))?;
            }
            SpanOp::CloseAgent {
                agent_id,
                status,
                attrs,
            } => {
                let key = format!("agent:{agent_id}");
                emitted.push(close(
                    store,
                    session,
                    trace_id,
                    &key,
                    &key,
                    SpanKind::Internal,
                    now_nanos,
                    status,
                    attrs,
                    None,
                )?);
            }
            SpanOp::Mark { name, attrs } => {
                let parent = SpanId::derive(session, &turn_key(store.current_turn(session)?));
                let key = format!("mark:{name}:{now_nanos}");
                emitted.push(Span {
                    trace_id,
                    span_id: SpanId::derive(session, &key),
                    parent_span_id: Some(parent),
                    name: name.clone(),
                    kind: SpanKind::Internal,
                    start_unix_nano: now_nanos,
                    end_unix_nano: now_nanos,
                    status: Status::Unset,
                    attributes: attrs.clone(),
                });
            }
        }
    }
    Ok(emitted)
}

fn turn_key(n: u64) -> String {
    format!("turn:{n}")
}

/// Stable tool span key: prefer the harness `tool_use_id`; without one, fall
/// back to the tool name (collides only for concurrent same-name anon tools - a
/// logged, bounded degradation, never a correctness hazard for keyed tools).
fn tool_key(tool_use_id: Option<&str>, tool_name: &str) -> String {
    match tool_use_id {
        Some(id) => format!("tool:{id}"),
        None => format!("tool:{tool_name}:anon"),
    }
}

#[allow(clippy::too_many_arguments)]
fn open(
    logical_key: &str,
    parent: Option<SpanId>,
    name: &str,
    kind: SpanKind,
    start_nanos: u64,
    attrs: &Attrs,
    session: &str,
) -> OpenSpan {
    OpenSpan {
        session_id: session.to_string(),
        logical_key: logical_key.to_string(),
        span_id: SpanId::derive(session, logical_key).to_hex(),
        parent_span_id: parent.map(|p| p.to_hex()),
        name: name.to_string(),
        kind: kind.as_otlp(),
        start_unix_nano: start_nanos,
        attrs_json: serde_json::to_string(attrs).unwrap_or_else(|_| "[]".into()),
    }
}

/// Build a completed span from its open record (if present) plus close info.
/// Falls back to a self-contained span when the open was never recorded.
#[allow(clippy::too_many_arguments)]
fn close(
    store: &Store,
    session: &str,
    trace_id: TraceId,
    logical_key: &str,
    name: &str,
    kind: SpanKind,
    end_nanos: u64,
    status: &Status,
    close_attrs: &Attrs,
    duration_ms: Option<u64>,
) -> Result<Span, gently_store::StoreError> {
    let opened = store.take_open(session, logical_key)?;

    // duration_ms (when the harness supplies it) is the most accurate width:
    // the gap between two separate hook processes includes hook overhead.
    let dur_nanos = duration_ms.map(|ms| ms.saturating_mul(1_000_000));
    let start_nanos = match (&opened, dur_nanos) {
        (_, Some(d)) => end_nanos.saturating_sub(d),
        (Some(o), None) => o.start_unix_nano,
        (None, None) => end_nanos, // missed the open: instant span, still valid
    };

    let mut attributes: Attrs = opened
        .as_ref()
        .and_then(|o| serde_json::from_str(&o.attrs_json).ok())
        .unwrap_or_default();
    attributes.extend(close_attrs.iter().cloned());

    let (span_id, parent, span_kind, span_name) = match &opened {
        Some(o) => (
            SpanId::from_hex(&o.span_id).unwrap_or_else(|| SpanId::derive(session, logical_key)),
            o.parent_span_id.as_deref().and_then(SpanId::from_hex),
            SpanKind::from_otlp(o.kind),
            o.name.clone(),
        ),
        None => (
            SpanId::derive(session, logical_key),
            None,
            kind,
            name.to_string(),
        ),
    };

    Ok(Span {
        trace_id,
        span_id,
        parent_span_id: parent,
        name: span_name,
        kind: span_kind,
        start_unix_nano: start_nanos,
        end_unix_nano: end_nanos,
        status: status.clone(),
        attributes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClaudeCode, Harness};
    use serde_json::json;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("state.db")).unwrap();
        (dir, s)
    }

    #[test]
    fn tool_span_parents_to_current_turn_and_uses_duration() {
        let (_d, s) = store();
        let h = ClaudeCode;

        // open a turn
        let p = h
            .parse(&json!({"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"hi"}))
            .unwrap();
        assert!(apply(&s, &p, 1_000).unwrap().is_empty());

        // pre tool
        let p = h
            .parse(&json!({"hook_event_name":"PreToolUse","session_id":"s",
            "tool_name":"Bash","tool_use_id":"tu_1","tool_input":{"command":"ls"}}))
            .unwrap();
        assert!(apply(&s, &p, 2_000).unwrap().is_empty());

        // post tool with duration_ms=1 (=1_000_000 ns)
        let p = h
            .parse(&json!({"hook_event_name":"PostToolUse","session_id":"s",
            "tool_name":"Bash","tool_use_id":"tu_1","tool_response":{"ok":true},"duration_ms":1}))
            .unwrap();
        let spans = apply(&s, &p, 5_000_000).unwrap();
        assert_eq!(spans.len(), 1);
        let span = &spans[0];
        assert_eq!(span.name, "Bash");
        assert_eq!(span.kind, SpanKind::Client);
        assert_eq!(span.status, Status::Ok);
        // duration_ms overrode the start
        assert_eq!(span.end_unix_nano - span.start_unix_nano, 1_000_000);
        // parent is turn:1
        let expected_parent = SpanId::derive("s", "turn:1");
        assert_eq!(span.parent_span_id, Some(expected_parent));
    }

    #[test]
    fn close_without_open_is_self_contained_instant_span() {
        let (_d, s) = store();
        let h = ClaudeCode;
        // PostToolUse with no preceding PreToolUse, no duration
        let p = h
            .parse(&json!({"hook_event_name":"PostToolUse","session_id":"s",
            "tool_name":"Read","tool_use_id":"tu_x","tool_response":{}}))
            .unwrap();
        let spans = apply(&s, &p, 9_000).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start_unix_nano, 9_000);
        assert_eq!(spans[0].end_unix_nano, 9_000);
        // still belongs to the trace
        assert_eq!(spans[0].trace_id, TraceId::from_session("s"));
    }

    #[test]
    fn session_root_emitted_provisionally_on_start_and_finalized_on_end() {
        let (_d, s) = store();
        let h = ClaudeCode;
        let root_id = SpanId::derive("s", "session");

        // SessionStart emits a provisional root immediately (start == end).
        let spans = apply(
            &s,
            &h.parse(&json!({"hook_event_name":"SessionStart","session_id":"s"})).unwrap(),
            100,
        )
        .unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].span_id, root_id);
        assert_eq!(spans[0].parent_span_id, None);
        assert_eq!(spans[0].start_unix_nano, 100);
        assert_eq!(spans[0].end_unix_nano, 100);

        // SessionEnd re-emits the same id spanning the full session.
        let spans = apply(
            &s,
            &h.parse(&json!({"hook_event_name":"SessionEnd","session_id":"s"})).unwrap(),
            900,
        )
        .unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].span_id, root_id, "same deterministic id => idempotent replace");
        assert_eq!(spans[0].start_unix_nano, 100);
        assert_eq!(spans[0].end_unix_nano, 900);
    }

    #[test]
    fn turn_span_emitted_on_stop() {
        let (_d, s) = store();
        let h = ClaudeCode;
        apply(
            &s,
            &h.parse(&json!({"hook_event_name":"UserPromptSubmit","session_id":"s"}))
                .unwrap(),
            100,
        )
        .unwrap();
        let spans = apply(
            &s,
            &h.parse(&json!({"hook_event_name":"Stop","session_id":"s"}))
                .unwrap(),
            500,
        )
        .unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "turn:1");
        assert_eq!(spans[0].start_unix_nano, 100);
        assert_eq!(spans[0].end_unix_nano, 500);
        assert_eq!(
            spans[0].parent_span_id,
            Some(SpanId::derive("s", "session"))
        );
    }
}
