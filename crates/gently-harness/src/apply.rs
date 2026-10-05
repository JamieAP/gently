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
    let root = &parsed.session_id;
    // Scope counters and keyed tools to the execution agent while retaining the
    // harness session's trace ID. Length-prefixing avoids ambiguous scopes.
    let scope = parsed
        .agent_id
        .as_ref()
        .map(|id| format!("{}:{root}:agent:{}:{id}", root.len(), id.len()));
    let session = scope.as_deref().unwrap_or(root);
    let trace_id = TraceId::from_session(root);
    let context_parent = parsed
        .agent_id
        .as_ref()
        .map(|id| SpanId::derive(root, &format!("agent:{id}")))
        .unwrap_or_else(|| SpanId::derive(root, "session"));
    let session_key = parsed
        .agent_id
        .as_ref()
        .map(|id| format!("agent:{id}"))
        .unwrap_or_else(|| "session".into());
    let mut emitted = Vec::new();

    for op in &parsed.ops {
        match op {
            SpanOp::OpenSession { attrs } => {
                emitted.push(open_provisional(
                    store,
                    root,
                    trace_id,
                    &session_key,
                    parsed
                        .agent_id
                        .as_ref()
                        .map(|_| SpanId::derive(root, "session")),
                    &session_key,
                    SpanKind::Internal,
                    now_nanos,
                    attrs,
                )?);
            }
            SpanOp::CloseSession { status, attrs } => {
                emitted.push(close(
                    store,
                    root,
                    trace_id,
                    &session_key,
                    &session_key,
                    SpanKind::Internal,
                    now_nanos,
                    status,
                    attrs,
                    None,
                )?);
            }
            SpanOp::OpenTurn { attrs } => {
                let (key, name, _) = resolve_turn(store, session, parsed.turn_id.as_deref(), true)?;
                let parent = context_parent;
                // Provisional turn so an interrupted turn (no Stop) still appears;
                // CloseTurn finalizes it via the same deterministic id.
                emitted.push(open_provisional(
                    store,
                    session,
                    trace_id,
                    &key,
                    Some(parent),
                    &name,
                    SpanKind::Internal,
                    now_nanos,
                    attrs,
                )?);
            }
            SpanOp::CloseTurn { status, attrs } => {
                let (key, name, _) =
                    resolve_turn(store, session, parsed.turn_id.as_deref(), false)?;
                let mut span = close(
                    store,
                    session,
                    trace_id,
                    &key,
                    &name,
                    SpanKind::Internal,
                    now_nanos,
                    status,
                    attrs,
                    None,
                )?;
                span.parent_span_id = span.parent_span_id.or(Some(context_parent));
                emitted.push(span);
            }
            SpanOp::OpenTool {
                tool_use_id,
                tool_name,
                attrs,
            } => {
                let key = tool_key(tool_use_id.as_deref(), tool_name);
                let (turn_lkey, turn_name, first_sight) =
                    resolve_turn(store, session, parsed.turn_id.as_deref(), false)?;
                ensure_turn_span(
                    store,
                    session,
                    trace_id,
                    context_parent,
                    &turn_lkey,
                    &turn_name,
                    now_nanos,
                    first_sight,
                    &mut emitted,
                )?;
                let parent = SpanId::derive(session, &turn_lkey);
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
                // Post hooks can be the first observable tool event. Resolve
                // an inferred turn so these spans remain attached to the tree.
                let (turn_lkey, turn_name, first_sight) =
                    resolve_turn(store, session, parsed.turn_id.as_deref(), false)?;
                ensure_turn_span(
                    store,
                    session,
                    trace_id,
                    context_parent,
                    &turn_lkey,
                    &turn_name,
                    now_nanos,
                    first_sight,
                    &mut emitted,
                )?;
                let mut span = close(
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
                )?;
                span.parent_span_id = span
                    .parent_span_id
                    .or(Some(SpanId::derive(session, &turn_lkey)));
                emitted.push(span);
            }
            SpanOp::OpenAgent {
                agent_id,
                parent_tool_use_id,
                attrs,
            } => {
                let key = format!("agent:{agent_id}");
                let parent = match parent_tool_use_id {
                    Some(tu) => {
                        let tool_key = format!("tool:{tu}");
                        // Lifecycle agent_id names the child, so its caller can
                        // be any execution scope in this root session. Use a
                        // known parent only when the reference is unambiguous.
                        store
                            .unique_open_span_id_in_scopes(
                                root,
                                &format!("{}:{root}:agent:", root.len()),
                                &tool_key,
                            )?
                            .as_deref()
                            .and_then(SpanId::from_hex)
                            .unwrap_or_else(|| SpanId::derive(session, &tool_key))
                    }
                    None => {
                        let (turn_lkey, turn_name, first_sight) =
                            resolve_turn(store, session, parsed.turn_id.as_deref(), false)?;
                        ensure_turn_span(
                            store,
                            session,
                            trace_id,
                            context_parent,
                            &turn_lkey,
                            &turn_name,
                            now_nanos,
                            first_sight,
                            &mut emitted,
                        )?;
                        SpanId::derive(session, &turn_lkey)
                    }
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
                let mut span = close(
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
                )?;
                if span.parent_span_id.is_none() {
                    let (turn_lkey, turn_name, first_sight) =
                        resolve_turn(store, session, parsed.turn_id.as_deref(), false)?;
                    ensure_turn_span(
                        store,
                        session,
                        trace_id,
                        context_parent,
                        &turn_lkey,
                        &turn_name,
                        now_nanos,
                        first_sight,
                        &mut emitted,
                    )?;
                    span.parent_span_id = Some(SpanId::derive(session, &turn_lkey));
                }
                emitted.push(span);
            }
            SpanOp::Mark { name, attrs } => {
                let (turn_lkey, turn_name, first_sight) =
                    resolve_turn(store, session, parsed.turn_id.as_deref(), false)?;
                ensure_turn_span(
                    store,
                    session,
                    trace_id,
                    context_parent,
                    &turn_lkey,
                    &turn_name,
                    now_nanos,
                    first_sight,
                    &mut emitted,
                )?;
                let parent = SpanId::derive(session, &turn_lkey);
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

/// Resolve the (logical_key, display_name) of the turn an op belongs to. When
/// the harness supplies a turn id (Codex), the turn span is keyed by that stable
/// id and named by its per-session ordinal, so out-of-order `Stop`s and tool
/// events resolve the correct turn. Otherwise (Claude) we fall back to the
/// monotonic counter, where key and name coincide. `opening` only affects the
/// counter path: a new turn advances the counter; other ops read the current.
fn resolve_turn(
    store: &Store,
    session: &str,
    turn_id: Option<&str>,
    opening: bool,
) -> Result<(String, String, bool), gently_store::StoreError> {
    match turn_id {
        Some(tid) => {
            let (ordinal, first_sight) = store.turn_ordinal(session, tid)?;
            Ok((
                format!("turn:{tid}"),
                format!("turn:{ordinal}"),
                first_sight,
            ))
        }
        None => {
            // Claude's counter turns are always opened explicitly by
            // `UserPromptSubmit`, which fires reliably - so there is no
            // first-sight-via-a-tool case to back-fill here.
            let n = if opening {
                store.next_turn_index(session)?
            } else {
                store.current_turn(session)?
            };
            Ok((turn_key(n), turn_key(n), false))
        }
    }
}

/// Lazily emit a provisional turn span the first time a turn is *referenced* by a
/// tool/agent/mark, not only when `UserPromptSubmit` opens it. Codex auto-starts
/// continuation turns (a new `task_started` right after the previous
/// `task_complete`, with no user input) that fire no `UserPromptSubmit` hook - so
/// without this, every tool under such a turn parents to a `turn:<id>` span that
/// never exists and dangles. The span is marked `TurnInferred`; the collector
/// may derive display bounds from available child observations. Those bounds
/// do not prove capture completeness or actual turn completion.
#[allow(clippy::too_many_arguments)]
fn ensure_turn_span(
    store: &Store,
    session: &str,
    trace_id: TraceId,
    parent: SpanId,
    turn_lkey: &str,
    turn_name: &str,
    now_nanos: u64,
    first_sight: bool,
    emitted: &mut Vec<Span>,
) -> Result<(), gently_store::StoreError> {
    if !first_sight {
        return Ok(());
    }
    let attrs: Attrs = vec![("gently.event".to_string(), "TurnInferred".to_string())];
    emitted.push(open_provisional(
        store,
        session,
        trace_id,
        turn_lkey,
        Some(parent),
        turn_name,
        SpanKind::Internal,
        now_nanos,
        &attrs,
    )?);
    Ok(())
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
    let existing_attrs = store
        .peek_open(session, logical_key)?
        .and_then(|s| serde_json::from_str(&s.attrs_json).ok())
        .unwrap_or_default();
    let attrs = merge_attrs(existing_attrs, attrs);
    let opened = store.reopen_provisional(&open(
        logical_key,
        parent,
        name,
        kind,
        now_nanos,
        &attrs,
        session,
    ))?;
    Ok(Span {
        trace_id,
        span_id: SpanId::derive(session, logical_key),
        parent_span_id: opened.parent_span_id.as_deref().and_then(SpanId::from_hex),
        name: name.to_string(),
        kind,
        start_unix_nano: opened.start_unix_nano,
        end_unix_nano: opened.start_unix_nano,
        status: Status::Unset,
        attributes: attrs,
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
    fn codex_turns_keyed_by_turn_id_survive_out_of_order_stop() {
        let (_d, s) = store();
        let h = Codex;
        // Two prompts → two turns, ids "a" then "b".
        let a = apply(
            &s,
            &h.parse(
                &json!({"hook_event_name":"UserPromptSubmit","session_id":"cx","turn_id":"a"}),
            )
            .unwrap(),
            100,
        )
        .unwrap();
        assert_eq!(a[0].name, "turn:1");
        let b = apply(
            &s,
            &h.parse(
                &json!({"hook_event_name":"UserPromptSubmit","session_id":"cx","turn_id":"b"}),
            )
            .unwrap(),
            200,
        )
        .unwrap();
        assert_eq!(b[0].name, "turn:2");

        // A tool in turn "b" parents to turn "b" - NOT merely the latest counter.
        apply(&s, &h.parse(&json!({"hook_event_name":"PreToolUse","session_id":"cx","turn_id":"b","tool_name":"Bash","tool_use_id":"t1","tool_input":{}})).unwrap(), 250).unwrap();
        let tool = apply(&s, &h.parse(&json!({"hook_event_name":"PostToolUse","session_id":"cx","turn_id":"b","tool_name":"Bash","tool_use_id":"t1","tool_response":{}})).unwrap(), 300).unwrap();
        assert_eq!(tool[0].parent_span_id, Some(SpanId::derive("cx", "turn:b")));

        // A Stop carrying turn_id "a" closes turn "a" specifically - the bug was
        // that it closed the latest turn ("b") via the counter.
        let close_a = apply(
            &s,
            &h.parse(&json!({"hook_event_name":"Stop","session_id":"cx","turn_id":"a"}))
                .unwrap(),
            400,
        )
        .unwrap();
        assert_eq!(close_a[0].span_id, SpanId::derive("cx", "turn:a"));
        assert_eq!(close_a[0].name, "turn:1");
        assert_eq!(close_a[0].start_unix_nano, 100);
        assert_eq!(close_a[0].end_unix_nano, 400);
        assert_eq!(close_a[0].status, Status::Ok);
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
    fn codex_tool_in_auto_turn_without_prompt_synthesizes_turn_span() {
        let (_d, s) = store();
        let h = Codex;
        // An auto-continuation turn fires NO UserPromptSubmit; a tool is the first
        // event to name it. Without back-fill the tool would parent to a turn span
        // that never exists (the dangling-parent bug).
        let pre = apply(
            &s,
            &h.parse(&json!({"hook_event_name":"PreToolUse","session_id":"cx",
                "turn_id":"auto1","tool_name":"Bash","tool_use_id":"t1","tool_input":{}}))
                .unwrap(),
            100,
        )
        .unwrap();
        assert_eq!(pre.len(), 1, "first-sight tool back-fills its turn span");
        assert_eq!(pre[0].name, "turn:1");
        assert_eq!(pre[0].span_id, SpanId::derive("cx", "turn:auto1"));
        assert_eq!(
            pre[0].parent_span_id,
            Some(SpanId::derive("cx", "session")),
            "inferred turn parents to the session root"
        );

        // The tool closes and parents to the now-existing turn - not a dangling id.
        let post = apply(
            &s,
            &h.parse(&json!({"hook_event_name":"PostToolUse","session_id":"cx",
                "turn_id":"auto1","tool_name":"Bash","tool_use_id":"t1","tool_response":{}}))
                .unwrap(),
            200,
        )
        .unwrap();
        assert_eq!(
            post[0].parent_span_id,
            Some(SpanId::derive("cx", "turn:auto1"))
        );

        // A second tool in the same turn does NOT re-emit the turn span.
        let pre2 = apply(
            &s,
            &h.parse(&json!({"hook_event_name":"PreToolUse","session_id":"cx",
                "turn_id":"auto1","tool_name":"Read","tool_use_id":"t2","tool_input":{}}))
                .unwrap(),
            300,
        )
        .unwrap();
        assert!(
            pre2.is_empty(),
            "turn already exists; no duplicate turn span"
        );
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
    #[test]
    fn resumed_session_keeps_original_start() {
        let (_d, store) = store();
        let h = ClaudeCode;
        for (now, source) in [(100, "startup"), (500, "compact")] {
            let parsed = h.parse(&json!({"hook_event_name":"SessionStart", "session_id":"resume", "source":source})).unwrap();
            apply(&store, &parsed, now).unwrap();
        }
        let parsed = h
            .parse(
                &json!({"hook_event_name":"SessionEnd", "session_id":"resume", "reason":"other"}),
            )
            .unwrap();
        let spans = apply(&store, &parsed, 1000).unwrap();
        assert_eq!(spans[0].start_unix_nano, 100);
        assert_eq!(spans[0].end_unix_nano, 1000);
    }

    #[test]
    fn child_turns_and_same_id_tools_are_isolated_from_root() {
        let (_d, store) = store();
        let h = ClaudeCode;
        let events = [
            (
                100,
                json!({"hook_event_name":"UserPromptSubmit","session_id":"root","prompt_id":"root-prompt"}),
            ),
            (
                110,
                json!({"hook_event_name":"SubagentStart","session_id":"root","agent_id":"child"}),
            ),
            (
                120,
                json!({"hook_event_name":"UserPromptSubmit","session_id":"root","prompt_id":"child-prompt","agent_id":"child"}),
            ),
            (
                200,
                json!({"hook_event_name":"PreToolUse","session_id":"root","prompt_id":"root-prompt","tool_name":"Bash","tool_use_id":"shared"}),
            ),
            (
                300,
                json!({"hook_event_name":"PreToolUse","session_id":"root","prompt_id":"child-prompt","agent_id":"child","tool_name":"Write","tool_use_id":"shared"}),
            ),
        ];
        let mut child_turn = None;
        for (now, raw) in events {
            let parsed = h.parse(&raw).unwrap();
            let spans = apply(&store, &parsed, now).unwrap();
            if now == 120 {
                child_turn = spans.into_iter().find(|s| s.name.starts_with("turn:"));
            }
        }
        let child_turn = child_turn.unwrap();
        assert_eq!(child_turn.name, "turn:1");
        assert_eq!(
            child_turn.parent_span_id,
            Some(SpanId::derive("root", "agent:child"))
        );
        let root = h.parse(&json!({"hook_event_name":"PostToolUse","session_id":"root","prompt_id":"root-prompt","tool_name":"Bash","tool_use_id":"shared"})).unwrap();
        let root_spans = apply(&store, &root, 500).unwrap();
        let root_tool = root_spans.iter().find(|s| s.name == "Bash").unwrap();
        assert_eq!(root_tool.start_unix_nano, 200);
        let child = h.parse(&json!({"hook_event_name":"PostToolUse","session_id":"root","prompt_id":"child-prompt","agent_id":"child","tool_name":"Write","tool_use_id":"shared"})).unwrap();
        let child_spans = apply(&store, &child, 600).unwrap();
        let child_tool = child_spans.iter().find(|s| s.name == "Write").unwrap();
        assert_eq!(child_tool.start_unix_nano, 300);
        assert_ne!(root_tool.span_id, child_tool.span_id);
        assert_eq!(root_tool.trace_id, child_tool.trace_id);
        assert_eq!(child_tool.parent_span_id, Some(child_turn.span_id));
    }

    #[test]
    fn post_tool_without_pre_still_has_a_turn_parent() {
        let (_d, store) = store();
        let parsed = Codex.parse(&json!({"hook_event_name":"PostToolUse","session_id":"late","turn_id":"turn-id","tool_name":"Bash","tool_use_id":"tool-id","tool_response":"opaque output"})).unwrap();
        let spans = apply(&store, &parsed, 1000).unwrap();
        let tool = spans.iter().find(|s| s.name == "Bash").unwrap();
        let turn = spans.iter().find(|s| s.name == "turn:1").unwrap();
        assert_eq!(tool.parent_span_id, Some(turn.span_id));
    }

    #[test]
    fn missing_subagent_start_still_attaches_child_tree_to_root_turn() {
        let (_d, s) = store();
        let h = ClaudeCode;
        let root = apply(&s, &h.parse(&json!({"hook_event_name":"UserPromptSubmit","session_id":"s","prompt_id":"parent"})).unwrap(), 10).unwrap();
        let child = apply(&s, &h.parse(&json!({"hook_event_name":"UserPromptSubmit","session_id":"s","agent_id":"child","prompt_id":"child-p"})).unwrap(), 20).unwrap();
        let ended = apply(&s, &h.parse(&json!({"hook_event_name":"SubagentStop","session_id":"s","agent_id":"child","prompt_id":"parent"})).unwrap(), 50).unwrap();
        let agent = ended.iter().find(|s| s.name == "agent:child").unwrap();
        assert_eq!(agent.parent_span_id, Some(root[0].span_id));
        assert_eq!(child[0].parent_span_id, Some(agent.span_id));
    }

    #[test]
    fn child_scope_and_turn_boundaries_do_not_collide() {
        let (_d, s) = store();
        let h = ClaudeCode;
        let first = apply(&s, &h.parse(&json!({"hook_event_name":"UserPromptSubmit","session_id":"s","agent_id":"child:turn","prompt_id":"prompt"})).unwrap(), 10).unwrap();
        let second = apply(&s, &h.parse(&json!({"hook_event_name":"UserPromptSubmit","session_id":"s","agent_id":"child","prompt_id":"turn:prompt"})).unwrap(), 20).unwrap();
        assert_ne!(first[0].span_id, second[0].span_id);
    }

    #[test]
    fn nested_subagent_uses_scoped_parent_tool_in_the_same_trace() {
        let (_d, store) = store();
        let h = ClaudeCode;
        let root = "r_%";
        for (now, raw) in [
            (
                10,
                json!({"hook_event_name":"UserPromptSubmit","session_id":root,"prompt_id":"root-p"}),
            ),
            (
                20,
                json!({"hook_event_name":"SubagentStart","session_id":root,"agent_id":"child","prompt_id":"root-p"}),
            ),
            (
                30,
                json!({"hook_event_name":"UserPromptSubmit","session_id":root,"agent_id":"child","prompt_id":"child-p"}),
            ),
            (
                40,
                json!({"hook_event_name":"PreToolUse","session_id":root,"agent_id":"child","prompt_id":"child-p","tool_name":"Agent","tool_use_id":"parent-tool"}),
            ),
            // Identical references in other roots must not make this match ambiguous.
            (
                45,
                json!({"hook_event_name":"PreToolUse","session_id":"rXX","agent_id":"child","prompt_id":"other-p","tool_name":"Agent","tool_use_id":"parent-tool"}),
            ),
            (
                46,
                json!({"hook_event_name":"PreToolUse","session_id":"r_%:other","prompt_id":"other-p","tool_name":"Agent","tool_use_id":"parent-tool"}),
            ),
        ] {
            apply(&store, &h.parse(&raw).unwrap(), now).unwrap();
        }
        let started = apply(
            &store,
            &h.parse(&json!({
                "hook_event_name":"SubagentStart", "session_id":root,
                "agent_id":"grandchild", "tool_use_id":"parent-tool", "prompt_id":"child-p"
            }))
            .unwrap(),
            50,
        )
        .unwrap();
        let grandchild = started
            .iter()
            .find(|span| span.name == "agent:grandchild")
            .unwrap();
        let child_scope = format!("{}:{root}:agent:5:child", root.len());
        assert_eq!(
            grandchild.parent_span_id,
            Some(SpanId::derive(&child_scope, "tool:parent-tool"))
        );
        assert_eq!(grandchild.trace_id, TraceId::from_session(root));

        let descendants = apply(
            &store,
            &h.parse(&json!({
                "hook_event_name":"UserPromptSubmit", "session_id":root,
                "agent_id":"grandchild", "prompt_id":"grandchild-p"
            }))
            .unwrap(),
            60,
        )
        .unwrap();
        let turn = descendants
            .iter()
            .find(|span| span.name == "turn:1")
            .unwrap();
        assert_eq!(turn.parent_span_id, Some(grandchild.span_id));
        assert_eq!(turn.trace_id, grandchild.trace_id);

        // Lookup must not consume the parent: its later close keeps start and parent.
        let completed = apply(
            &store,
            &h.parse(&json!({
                "hook_event_name":"PostToolUse", "session_id":root,
                "agent_id":"child", "prompt_id":"child-p",
                "tool_name":"Agent", "tool_use_id":"parent-tool"
            }))
            .unwrap(),
            70,
        )
        .unwrap();
        let tool = completed.iter().find(|span| span.name == "Agent").unwrap();
        assert_eq!(grandchild.parent_span_id, Some(tool.span_id));
        assert_eq!(tool.start_unix_nano, 40);
    }

    #[test]
    fn ambiguous_parent_tool_reference_keeps_root_fallback() {
        let (_d, store) = store();
        let h = Codex;
        for (now, agent) in [(10, Some("child")), (20, Some("sibling"))] {
            let raw = json!({"hook_event_name":"PreToolUse","session_id":"root",
                "agent_id":agent,"turn_id":"prompt","tool_name":"Agent","tool_use_id":"shared"});
            apply(&store, &h.parse(&raw).unwrap(), now).unwrap();
        }
        let spans = apply(
            &store,
            &h.parse(&json!({
                "hook_event_name":"SubagentStart","session_id":"root", "agent_id":"grandchild",
                "tool_use_id":"shared","turn_id":"prompt"
            }))
            .unwrap(),
            30,
        )
        .unwrap();
        let agent = spans
            .iter()
            .find(|span| span.name == "agent:grandchild")
            .unwrap();
        assert_eq!(
            agent.parent_span_id,
            Some(SpanId::derive("root", "tool:shared"))
        );
    }

    #[test]
    fn missing_parent_tool_reference_keeps_root_fallback() {
        let (_d, store) = store();
        let parsed = ClaudeCode
            .parse(&json!({
                "hook_event_name":"SubagentStart","session_id":"root", "agent_id":"child",
                "tool_use_id":"missing"
            }))
            .unwrap();
        let spans = apply(&store, &parsed, 10).unwrap();
        assert_eq!(
            spans[0].parent_span_id,
            Some(SpanId::derive("root", "tool:missing"))
        );
    }

    #[test]
    fn parent_tool_lookup_uses_the_stored_span_identifier() {
        let (_d, store) = store();
        let stored = SpanId::derive("stored", "parent");
        store
            .open_span(&OpenSpan {
                session_id: "root".into(),
                logical_key: "tool:parent-tool".into(),
                span_id: stored.to_hex(),
                parent_span_id: None,
                name: "Agent".into(),
                kind: SpanKind::Client as u8,
                start_unix_nano: 1,
                attrs_json: "[]".into(),
            })
            .unwrap();
        let parsed = ClaudeCode
            .parse(&json!({
                "hook_event_name":"SubagentStart", "session_id":"root",
                "agent_id":"child", "tool_use_id":"parent-tool"
            }))
            .unwrap();
        let spans = apply(&store, &parsed, 10).unwrap();
        assert_eq!(spans[0].parent_span_id, Some(stored));
    }
}
