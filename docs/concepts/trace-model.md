# Trace model

One trace per harness session. Spans are named `session`, `turn:N`, the tool name
(`Bash`, `Read`, …), or `agent:<id>`.

```
session                         SessionStart → SessionEnd
└─ turn:N                       UserPromptSubmit → Stop
   ├─ tool:<id>                 PreToolUse → PostToolUse
   ├─ tool:<id>
   └─ agent:<id>                SubagentStart → SubagentStop
      └─ turn:N                 subagent prompt/turn context
         └─ tool:<id>
```

## Deterministic ids

Ids are pure functions of harness identifiers - the core design choice:

* `trace_id = blake3(session_id)[..16]`
* `span_id  = blake3(session_id || ":" || logical_key)[..8]`, where `logical_key`
  uses the harness turn/prompt ID when available, otherwise `"turn:N"`; tools
  use `"tool:<tool_use_id>"` and agents use `"agent:<agent_id>"`. Subagent turn and
  tool keys include execution-agent context so they cannot collide with the
  main thread.

Claude uses `prompt_id` when present; older Claude payloads retain the monotonic
turn-counter fallback. Codex uses `turn_id`. `agent_id` on an ordinary hook names
the executing subagent; on `SubagentStart`/`SubagentStop` it names the lifecycle
subject. The root session trace is shared, while subagent turns and tools parent
under that agent. A resume or compact continuation keeps the original session
start rather than reopening it at a later timestamp. Lifecycle hooks identify
the child rather than its caller. A unique parent-tool reference is resolved
against open tools throughout that root session, including subagent scopes.
Without an unambiguous reference, nested caller context cannot be reconstructed;
the [documented fallback](../reference/hooks.md) retains root-level parentage.

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
transparency. The same fallback applies when any current hook references a
previously unseen prompt/turn ID, including subagent work.

## Effective bounds

A parent's own hook events may not enclose the observed activity. The collector
returns derived **`effective_start` / `effective_end`** display bounds alongside
the raw `start` / `end`:

* **Parentless record:** `MIN(start)` / `MAX(end)` over the whole recorded trace.
* **Other record:** the minimum start from itself and its direct children. An
  existing non-provisional end is retained; otherwise, the end uses the observed
  maximum among direct children, falling back to the record's start.

Two aggregates compute these bounds without recursion. The CLI waterfall and other
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
`source` (SessionStart), `effort`, safe `reason` (SessionEnd), `agent_type`,
`agent_transcript_path` (SubagentStop), prompt/agent context, compaction trigger,
interrupt/error category and model-switch metadata. **Digests** use `…sha256`
(first 8 bytes) plus `…bytes` for prompt/tool/assistant values and content-bearing
Claude diagnostics, compaction text and batches. No raw content; see
[Security & privacy](security-and-privacy.md).

## Durations are real

A turn's duration is mostly the model thinking between and after tool calls; the
tools are short spans nested inside it. The waterfall reflects genuine wall-clock,
so a turn can extend past its last tool up to `Stop`. Claude `StopFailure` closes
a failed turn; Codex `Interrupt` closes an interrupted turn with unset status.
Codex opaque tool outputs also keep unset status; typed MCP `isError` results
provide explicit success or failure. See [Harness hooks](../reference/hooks.md).
