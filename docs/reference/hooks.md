# Harness hooks

This table describes the adapters and default registrations shipped in this
checkout. Synthetic fixtures exercise selected payloads and lifecycle cases;
they do not establish compatibility with every release or desktop integration.
See the [checked compatibility baseline](compatibility.md) when upgrading.

The [Claude hook reference](https://code.claude.com/docs/en/hooks) and
[Codex hook documentation](https://learn.chatgpt.com/docs/hooks) describe their
respective products. Re-run `gently init --claude` or `gently init --codex` after
updating Gently to register newly supported events, then restart the agent.
Codex registrations also require the agent's hook trust step.

## Installed mappings

| Action | Claude Code | Codex |
| --- | --- | --- |
| Session open/close | `SessionStart` / `SessionEnd` | `SessionStart` / `SessionEnd` |
| Turn open/close | `UserPromptSubmit` / `Stop` | `UserPromptSubmit` / `Stop` |
| Failed or interrupted turn | `StopFailure` closes with error status. | `Interrupt` closes with unset status and an interruption attribute. |
| Tool open/close | `PreToolUse` / `PostToolUse`; `PostToolUseFailure` closes with error status. | `PreToolUse` / `PostToolUse` |
| Subagent open/close | `SubagentStart` / `SubagentStop` | `SubagentStart` / `SubagentStop` |
| Compaction markers | `PreCompact`, `PostCompact` | `PreCompact`, `PostCompact` |
| Turn markers | `PermissionRequest`, `PermissionDenied`, `PostToolBatch`, `UserPromptExpansion`, `TaskCreated`, `TaskCompleted`, `Elicitation`, `ElicitationResult` | `PermissionRequest` |
| Context markers | `Setup`, `Notification`, `InstructionsLoaded`, `ConfigChange`, `CwdChanged`, `DirectoryAdded`, `FileChanged`, `MessageDisplay`, `TeammateIdle`, `PreModelSwitch`, `PostModelSwitch` | — |

A turn marker attaches to the current or inferred turn. Context markers attach
to the session or executing subagent without inventing a turn. Unknown events
become turn markers if explicitly wired. Every successfully processed event
also emits an immutable `hook:<Event>` receipt with a complete payload byte length.
Opt-in encrypted capture and explicit reader resolution can retain and return
that normalized JSON.
Init registers 31 Claude and 12 Codex events. Claude worktree create/remove
handlers are intentionally excluded because they replace Git operations.

Both adapters require string `session_id` and `hook_event_name` fields. Other
missing fields may degrade correlation rather than reject the event. Malformed
JSON or missing required fields cannot produce a normal span record; the hook
contains processing errors, so its exit code is not a capture check.

## Turns and subagent execution

Claude `prompt_id` and Codex `turn_id` identify turns when present. Without them,
Gently uses local counters, which cannot reconstruct unseen prompt boundaries.
A first reference to an unseen prompt/turn ID can emit a `TurnInferred` parent;
see [Trace model](../concepts/trace-model.md#inferred-turns-and-resumes).

For ordinary Claude events, a nonempty `agent_id` identifies the executing
subagent. The Codex adapter applies that context to prompt, tool, permission,
stop and compaction events. Both keep subagent work within the root session's
trace. Lifecycle events use `agent_id` differently: it identifies the child
being opened or closed. A missing or empty lifecycle agent ID produces a marker
rather than an invented subagent span.

### Subagent parentage

On `SubagentStart`, parent selection depends on the payload and local state:

- A supplied `tool_use_id` links to that open tool when exactly one match exists
  across the root session's main and subagent scopes.
- A missing or ambiguous match falls back to a tool ID in the root scope. That
  parent can be absent from the collected trace.
- Without `tool_use_id`, the subagent attaches to a root turn, inferred from a
  supplied prompt/turn ID when needed.

A matching local open record supplies parentage on close. If that record is
missing, the close falls back to turn parentage. These fallbacks preserve a
record but cannot always recover nested caller context from incomplete payloads.

## Status and content handling

Tool aggregates retain the supplied invocation ID in `gently.tool_use_id`, so
the collector's `tool_use_id` query column can correlate opens and closes.
Missing or empty IDs use the documented tool-name fallback; an empty subagent
caller ID uses turn parentage instead of inventing a tool reference.
Permission markers retain namespaced `gently.hook.tool_name` and
`gently.hook.tool_use_id` metadata plus an input byte length. They are observations,
so they do not add tool executions to usage or duration rollups. Historical
permission markers remain queryable and are also excluded from those rollups.

Claude `PostToolUse` maps to success; `PostToolUseFailure` maps to error. Codex
opaque tool results remain unset. A Codex tool named with the `mcp__` prefix and
a structured result containing a `content` array is treated as an MCP result:
boolean `isError: true` means error; false or absent means success. Malformed
`isError` values remain unset. Arbitrary shell output is not interpreted as a
success or failure signal.

Claude batch events create one marker instead of repeating individual tool
closes. Content-bearing tool diagnostics, API failure details, compaction and
batch values emit byte lengths and can be retained only as encrypted raw
fields. Public content hashes are not emitted. Recognized error categories and selected lifecycle
metadata may remain readable; freeform details are not raw status text.

Encrypted capture and reader resolution are independent of
these adapter mappings. Read [Security and privacy](../concepts/security-and-privacy.md)
before enabling raw content options.

## Implementation references

- [Claude adapter and fixtures](https://github.com/JamieAP/gently/blob/main/crates/gently-harness/src/claude.rs)
- [Codex adapter and fixtures](https://github.com/JamieAP/gently/blob/main/crates/gently-harness/src/codex.rs)
- [Stateful span application](https://github.com/JamieAP/gently/blob/main/crates/gently-harness/src/apply.rs)
- [Default hook registration](https://github.com/JamieAP/gently/blob/main/crates/gently-cli/src/cmd_init.rs)
