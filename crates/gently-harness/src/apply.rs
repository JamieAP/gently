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
                emitted.push(open_provisional(
                    store,
                    session,
                    trace_id,
                    "session",
                    None,
                    "session",
                    SpanKind::Internal,
                    now_nanos,
                    attrs,
                )?);
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
                // Provisional turn so an interrupted turn (no Stop) still appears;
                // CloseTurn finalizes it via the same deterministic id.
                emitted.push(open_provisional(
                    store,
                    session,
                    trace_id,
                    &key,
                    Some(parent),
                    &key,
                    SpanKind::Internal,
                    now_nanos,
                    attrs,
                )?);
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
                // Provisional agent span so a subagent whose SubagentStop never
                // fires (unreliable per #7881) still appears; CloseAgent finalizes.
                emitted.push(open_provisional(
                    store,
                    session,
                    trace_id,
                    &key,
                    Some(parent),
                    &key,
                    SpanKind::Internal,
                    now_nanos,
                    attrs,
                )?);
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

/// Merge `base` with `overrides`, keeping keys unique. An overriding key
/// replaces the base value in place (preserving position); new keys append.
fn merge_attrs(mut base: Attrs, overrides: &Attrs) -> Attrs {
    for (k, v) in overrides {
        match base.iter_mut().find(|(ek, _)| ek == k) {
            Some(slot) => slot.1 = v.clone(),
            None => base.push((k.clone(), v.clone())),
        }
    }
    base
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

/// Record an open span in the store and return a provisional, self-contained
/// span (zero-duration, status Unset) to emit immediately. The matching `Close*`
/// later emits the same deterministic id with the real duration, which the
/// idempotent ingest overwrites - so an interrupted span (no close) still shows.
#[allow(clippy::too_many_arguments)]
fn open_provisional(
    store: &Store,
    session: &str,
    trace_id: TraceId,
    logical_key: &str,
    parent: Option<SpanId>,
    name: &str,
    kind: SpanKind,
    now_nanos: u64,
    attrs: &Attrs,
) -> Result<Span, gently_store::StoreError> {
    store.open_span(&open(
        logical_key,
        parent,
        name,
        kind,
        now_nanos,
        attrs,
        session,
    ))?;
    Ok(Span {
        trace_id,
        span_id: SpanId::derive(session, logical_key),
        parent_span_id: parent,
        name: name.to_string(),
        kind,
        start_unix_nano: now_nanos,
        end_unix_nano: now_nanos,
        status: Status::Unset,
        attributes: attrs.clone(),
    })
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

    let opened_attrs: Attrs = opened
        .as_ref()
        .and_then(|o| serde_json::from_str(&o.attrs_json).ok())
        .unwrap_or_default();
    // Merge the open-event attrs with the close-event attrs, deduping by key so
    // a paired span (e.g. PreToolUse + PostToolUse) carries unique keys; the
    // close (later) value wins, per the OTLP "attribute keys are unique" rule.
    let attributes = merge_attrs(opened_attrs, close_attrs);

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
    use crate::{ClaudeCode, Codex, Harness};
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

        // open a turn - emits a provisional turn span immediately
        let p = h
            .parse(&json!({"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"hi"}))
            .unwrap();
        let prov = apply(&s, &p, 1_000).unwrap();
        assert_eq!(prov.len(), 1);
        assert_eq!(prov[0].name, "turn:1");
        assert_eq!(prov[0].end_unix_nano, prov[0].start_unix_nano);

        // pre tool - tools stay close-only, so nothing emitted yet
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
            &h.parse(&json!({"hook_event_name":"SessionStart","session_id":"s"}))
                .unwrap(),
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
            &h.parse(&json!({"hook_event_name":"SessionEnd","session_id":"s"}))
                .unwrap(),
            900,
        )
        .unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].span_id, root_id,
            "same deterministic id => idempotent replace"
        );
        assert_eq!(spans[0].start_unix_nano, 100);
        assert_eq!(spans[0].end_unix_nano, 900);
    }

    #[test]
    fn paired_tool_span_has_unique_attribute_keys() {
        let (_d, s) = store();
        let h = ClaudeCode;
        apply(
            &s,
            &h.parse(&json!({"hook_event_name":"UserPromptSubmit","session_id":"s"}))
                .unwrap(),
            1,
        )
        .unwrap();
        apply(&s, &h.parse(&json!({"hook_event_name":"PreToolUse","session_id":"s",
            "tool_name":"Bash","tool_use_id":"tu_1","tool_input":{"command":"ls"},"permission_mode":"default"})).unwrap(), 2).unwrap();
        let spans = apply(&s, &h.parse(&json!({"hook_event_name":"PostToolUse","session_id":"s",
            "tool_name":"Bash","tool_use_id":"tu_1","tool_response":{"ok":true},"permission_mode":"default"})).unwrap(), 3).unwrap();
        let keys: Vec<&str> = spans[0]
            .attributes
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        let mut uniq = keys.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(
            keys.len(),
            uniq.len(),
            "no duplicate attribute keys: {keys:?}"
        );
        // close value wins
        let event: Vec<&str> = spans[0]
            .attributes
            .iter()
            .filter(|(k, _)| k == "gently.event")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(event, vec!["PostToolUse"]);
    }

    #[test]
    fn turn_and_agent_emit_provisional_on_open() {
        let (_d, s) = store();
        let h = ClaudeCode;

        // UserPromptSubmit -> provisional turn:1 (zero-duration, unset status)
        let spans = apply(
            &s,
            &h.parse(&json!({"hook_event_name":"UserPromptSubmit","session_id":"s"}))
                .unwrap(),
            10,
        )
        .unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "turn:1");
        assert_eq!(spans[0].span_id, SpanId::derive("s", "turn:1"));
        assert_eq!(spans[0].status, Status::Unset);
        assert_eq!(spans[0].start_unix_nano, spans[0].end_unix_nano);

        // SubagentStart -> provisional agent span
        let spans = apply(&s, &h.parse(&json!({"hook_event_name":"SubagentStart","session_id":"s","agent_id":"ag1","agent_type":"explore"})).unwrap(), 20).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "agent:ag1");
        assert_eq!(spans[0].span_id, SpanId::derive("s", "agent:ag1"));

        // Closing each finalizes the SAME id with real duration.
        let stop = apply(
            &s,
            &h.parse(&json!({"hook_event_name":"Stop","session_id":"s"}))
                .unwrap(),
            500,
        )
        .unwrap();
        assert_eq!(stop[0].span_id, SpanId::derive("s", "turn:1"));
        assert_eq!(stop[0].start_unix_nano, 10);
        assert_eq!(stop[0].end_unix_nano, 500);
        assert_eq!(stop[0].status, Status::Ok);
    }

    #[test]
    fn codex_session_without_end_keeps_provisional_root_and_parents_turn() {
        let (_d, s) = store();
        let h = Codex;

        // SessionStart -> provisional root (zero-width, Unset), id "session".
        let start = apply(
            &s,
            &h.parse(&json!({"hook_event_name":"SessionStart","session_id":"cx"}))
                .unwrap(),
            100,
        )
        .unwrap();
        assert_eq!(start.len(), 1);
        assert_eq!(start[0].span_id, SpanId::derive("cx", "session"));
        assert_eq!(start[0].parent_span_id, None);
        assert_eq!(start[0].start_unix_nano, start[0].end_unix_nano); // provisional

        // UserPromptSubmit -> turn parented to the session root.
        let turn = apply(
            &s,
            &h.parse(&json!({"hook_event_name":"UserPromptSubmit","session_id":"cx"}))
                .unwrap(),
            200,
        )
        .unwrap();
        assert_eq!(turn[0].name, "turn:1");
        assert_eq!(
            turn[0].parent_span_id,
            Some(SpanId::derive("cx", "session")),
            "turn parents to the provisional session root even though SessionEnd never fires"
        );

        // Stop -> turn finalized; session root is never re-emitted (no SessionEnd).
        let stop = apply(
            &s,
            &h.parse(&json!({"hook_event_name":"Stop","session_id":"cx"}))
                .unwrap(),
            900,
        )
        .unwrap();
        assert_eq!(stop[0].name, "turn:1");
        assert_eq!(stop[0].start_unix_nano, 200);
        assert_eq!(stop[0].end_unix_nano, 900);
        assert_eq!(stop[0].status, Status::Ok);
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
