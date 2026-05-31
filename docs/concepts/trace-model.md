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
→ idempotent replace, so the final span overwrites the provisional one; if the
close never fires, the provisional still anchors the tree. (Tool spans are
close-only - `PostToolUse` is reliable and fast.)

## What's in a span

| Field | Notes |
|---|---|
| `trace_id` / `span_id` / `parent_span_id` | deterministic, hex |
| `name` | `session` / `turn:N` / tool name / `agent:<id>` |
| `kind` | Internal (session/turn/agent) or Client (tool) |
| `start` / `end` unix-nanos | string-encoded on the wire |
| `status` | unset / ok / error |

**Resource attributes** (per session): `service.name`, `gently.harness`,
`gently.session_id`, `gently.cwd`, `host.name`, `os.type`, `gently.version`.

**Span attributes** (`gently.*`): `event`, `tool_name`, `tool_use_id`,
`permission_mode`, and **digests** - `…sha256` (first 8 bytes) + `…bytes` for
tool input/response and prompts. No raw content; see
[Security & privacy](security-and-privacy.md).

## Durations are real

A turn's duration is mostly the model thinking between and after tool calls; the
tools are short spans nested inside it. The waterfall reflects genuine wall-clock,
so a turn legitimately extends well past its last tool up to `Stop`.
