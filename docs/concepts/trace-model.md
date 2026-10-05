# Trace model

Gently groups activity by the harness's session ID. A trace contains a session
span, turns, tools and subagents. For example, a tool that launches a subagent
can produce this tree:

```text
session
└─ turn:1
   ├─ Bash
   └─ Agent
      └─ agent:worker
         └─ turn:1
            └─ Read
```

This is an illustrative hierarchy. Actual parentage depends on the identifiers
and lifecycle events supplied by the harness. Subagent turn numbering belongs
to its execution scope, so both main and subagent work can have a `turn:1`.

## Opening and closing spans

| Span | Open | Close | What is exported |
| --- | --- | --- | --- |
| Session | `SessionStart` | `SessionEnd` | A provisional report on open and a report with closing status on close. |
| Turn | `UserPromptSubmit`, or first reference to an unseen prompt/turn ID | `Stop`, Claude `StopFailure`, or Codex `Interrupt` | A provisional report, then a closing report when observed. |
| Tool | `PreToolUse` | `PostToolUse`, or Claude `PostToolUseFailure` | Local open bookkeeping; a span is exported on close. |
| Subagent | `SubagentStart` | `SubagentStop` | A provisional report, then a closing report when observed. |
| Marker | Compaction, permission and other marker events | Same event | An instant span attached to a turn. |

Provisional reports have equal start and end timestamps and unset status. The
closing report uses the same span ID. If a close is missing, an already emitted
provisional span may remain visible, but it does not establish a completion
time. A tool whose close never arrives is not exported as a tool span.

When a close arrives without local open bookkeeping, Gently still emits a span.
Its duration is zero unless the hook provides `duration_ms`. This fallback
cannot recover an unobserved start time.

## IDs and turn context

The trace ID is the first 16 bytes of BLAKE3 over the root session ID. Span IDs
use the first 8 bytes of BLAKE3 over an execution scope and logical key. Keys
include `session`, `turn:<id>`, `tool:<tool_use_id>` and `agent:<agent_id>`.
Main and subagent execution scopes distinguish their turns and tools.

Claude `prompt_id` and Codex `turn_id`, when present, identify turns across
separate hook invocations. A local first-seen ordinal supplies the display name
`turn:N`. Without a prompt/turn ID, Gently uses the local turn counter. A missing
`tool_use_id` falls back to the tool name, which can collide for concurrent
anonymous tools with the same name.

Deterministic IDs let reports for the same logical span merge and let a child
name a parent before that parent is delivered. They do not replace local state:
open timestamps, fallback counters and some parent resolution still depend on
bookkeeping. Missing events or lost state can leave gaps or ambiguous links.
See [OpenTelemetry format](../reference/otel-format.md#deliberate-deviations)
for the ID scheme's interoperability choices.

### Inferred turns and resumes

When a tool, agent or marker first refers to an unseen prompt/turn ID, Gently
emits a provisional turn with `gently.event = "TurnInferred"`. This covers
continuation work that has no observed `UserPromptSubmit`. The inferred start
is the time Gently first observed that reference, not a recovered prompt time.
The counter fallback without a prompt/turn ID does not infer the same parent.

A resume reuses the session's deterministic ID. Existing local open bookkeeping
preserves its earliest start; the collector also keeps the earliest reported
start when merging reports. No new trace is created solely for a resume.

Ordinary subagent hooks use `agent_id` as execution context. On
`SubagentStart` and `SubagentStop`, that field names the child being started or
stopped. Parent-tool linking and incomplete-payload fallbacks are described in
[Harness hooks](../reference/hooks.md#subagent-parentage).

## Effective bounds

`gently trace --json` returns raw `start_unix_nano` and `end_unix_nano` plus
query-time `effective_start_unix_nano` and `effective_end_unix_nano`. The latter
help render incomplete lifecycle records. The current collector derives them
as follows:

| Record | Effective start | Effective end |
| --- | --- | --- |
| No parent | Earliest raw start in the trace. | Latest raw end in the trace, using a row's start when its end is absent. |
| Has a parent | Earlier of its own raw start and its direct children's raw starts. | Its stored end if present and different from its start; otherwise the latest direct-child end, falling back to child starts or its own start. |

This uses trace and direct-child aggregates, not a recursive envelope of every
descendant. A non-provisional parent end is retained even when a child extends
past it. Derived bounds describe observed records; they do not supply missing
events or prove when an interrupted session actually ended.

The [CLI waterfall](../guides/querying-and-mcp.md#waterfall) prefers effective
bounds, falling back to raw fields when needed. Its negative-duration check
uses raw timestamps; nesting checks use render bounds with 2 ms of clock slack.
Passing these checks does not prove capture is complete.

## Reading durations and attributes

Timestamps normally come from the local wall clock when a hook is processed.
They include hook timing and scheduling effects. Claude tool `duration_ms`, when
provided, is used to derive that tool's start from its close timestamp. A turn
can include model work, tool activity and pauses; its width is not a measurement
of model computation alone.

Status describes the mapped hook result. Claude `StopFailure` closes a turn
with error status; Codex `Interrupt` closes it as unset with an interruption
attribute. Opaque Codex tool results remain unset. Neither a normal close nor
an `ok` status proves the task's result is correct.

Resources include the harness, session ID, working directory, transcript path,
host, operating system and Gently version. Span attributes include event/tool
IDs, context and model metadata when supplied, plus opaque references and byte lengths
for selected content fields. These fields can identify projects and activity.
See [Security and privacy](security-and-privacy.md) for exported data, local raw
capture and query resolution.
