# Trace model

One trace per harness session. Spans are named `session`, `turn:N`, the tool name
(`Bash`, `Read`, …), or `agent:<id>`.

```
session                         SessionStart → SessionEnd
└─ turn:N                       UserPromptSubmit → Stop
   ├─ tool:<id>                 PreToolUse → PostToolUse
   ├─ tool:<id>
   └─ agent:<id>                SubagentStart → SubagentStop (best-effort link)
```

## Deterministic ids

Ids are pure functions of harness identifiers - the core design choice:

* `trace_id = blake3(session_id)[..16]`
* `span_id  = blake3(session_id || ":" || logical_key)[..8]`, where `logical_key`
  is `"session"`, `"turn:N"`, `"tool:<tool_use_id>"`, or `"agent:<agent_id>"`.

Because ids are deterministic, a child span computes its `parent_span_id` without
the parent existing yet and without any surviving local state. This is what lets
separate, short-lived hook processes reconstruct one coherent tree, and what makes
ingest idempotent (re-sending a span is a no-op upsert).

> OpenTelemetry *recommends* random ids; gently's are deterministic by design.
> See [OTel format](../reference/otel-format.md#deliberate-deviations).

## Self-contained spans

No span depends on its parent being present. A session killed without
`SessionEnd` still renders - the backend assembles the tree purely from
`(trace_id, parent_span_id)`.

To survive crashes, **session, turn, and subagent spans emit a *provisional*
span on open** (zero-duration) and are finalized on close. Same deterministic id
→ idempotent merge, so the final span supersedes the provisional one; if the
close never fires, the provisional still anchors the tree. (Tool spans are
close-only - `PostToolUse` is reliable and fast.)

### Inferred turns

Codex auto-starts continuation turns - a new `task_started` fires the instant the
previous `task_complete` does, with no user input - so **no `UserPromptSubmit`
hook fires** and the turn is never opened the normal way. To keep such a turn's
tools from dangling, the applier **back-fills a provisional turn span the first
time *any* event references a turn** (a tool, subagent, or mark), not only
`UserPromptSubmit`. Inferred turns carry `gently.event = "TurnInferred"` for
transparency. (Claude opens every turn explicitly, so this never triggers there.)

## Effective bounds

A parent's own hook events may not enclose the observed activity. The collector
returns derived **`effective_start` / `effective_end`** display bounds alongside
the raw `start` / `end`:

* **Parentless record:** `MIN(start)` / `MAX(end)` over the whole recorded trace.
* **Other record:** the minimum start from itself and its direct children. An
  existing non-provisional end is retained; otherwise, the end uses the observed
  maximum among direct children, falling back to the record's start.

Two aggregates compute these bounds without recursion. `waterfall.py` and other
renderers use them for width and nesting while retaining raw bounds for checks.
They do not recursively enclose every descendant and do not prove complete
capture or actual session, turn or tool completion.

## What's in a span

| Field | Notes |
|---|---|
| `trace_id` / `span_id` / `parent_span_id` | deterministic, hex |
| `name` | `session` / `turn:N` / tool name / `agent:<id>` |
| `kind` | Internal (session/turn/agent) or Client (tool) |
| `start` / `end` unix-nanos | raw, string-encoded on the wire |
| `effective_start` / `effective_end` | derived render bounds (see above); query-time only |
| `status` | unset / ok / error |

**Resource attributes** (per session): `service.name`, `gently.harness`,
`gently.session_id`, `gently.cwd`, `gently.transcript_path`, `host.name`,
`os.type`, `gently.version`.

**Span attributes** (`gently.*`): `event`, `tool_name`, `tool_use_id`,
`permission_mode`, and - when the harness payload carries them - `model`,
`source` (SessionStart), `effort`, `reason` (SessionEnd), `agent_type`,
`agent_transcript_path` (SubagentStop). Plus **digests** - `…sha256` (first 8
bytes) + `…bytes` for tool input/response and prompts. No raw content; see
[Security & privacy](security-and-privacy.md).

## Durations are real

A turn's duration is mostly the model thinking between and after tool calls; the
tools are short spans nested inside it. The waterfall reflects genuine wall-clock,
so a turn legitimately extends well past its last tool up to `Stop`.
