# Harness hooks

Adapters are checked against Claude Code 2.1.285 and Codex CLI 0.160.0 using
synthetic payloads from the [Claude hook reference](https://code.claude.com/docs/en/hooks)
and [Codex hook contract](https://learn.chatgpt.com/docs/hooks).
Run `gently init` again after upgrading to register newly supported events.

| Mapping | Claude Code | Codex |
|---|---|---|
| Session open/close | `SessionStart` / `SessionEnd` | `SessionStart` / `SessionEnd` |
| Turn open/close | `UserPromptSubmit` / `Stop` | `UserPromptSubmit` / `Stop` |
| Failed/interrupted turn | `StopFailure` → error | `Interrupt` → unset, interrupted |
| Tool open/close | `PreToolUse` / `PostToolUse`; `PostToolUseFailure` → error | `PreToolUse` / `PostToolUse` |
| Agent open/close | `SubagentStart` / `SubagentStop` | `SubagentStart` / `SubagentStop` |
| Compaction markers | `PreCompact`, `PostCompact` | `PreCompact`, `PostCompact` |
| Other installed markers | `PermissionRequest`, `PostToolBatch`, `PostModelSwitch` | `PermissionRequest` |

Unknown events become markers when wired to Gently. Only `session_id` and
`hook_event_name` are required. Missing lifecycle agent IDs become markers.
Claude `prompt_id` and Codex `turn_id` correlate turns; legacy Claude falls back
to counters. Ordinary subagent hooks use execution `agent_id`; lifecycle hooks
identify their child subject instead. Both stay within the root session trace.
Resumes preserve the earliest session start.

A lifecycle event's optional parent `tool_use_id` links to a matching open tool
in the root or a subagent scope when that match is unique. This preserves nested
agents' actual parent tool. Without that reference, the event cannot identify
its caller's execution context and falls back to a root turn; missing or
ambiguous tool references retain the root-tool fallback. Deep nesting cannot
always be reconstructed from incomplete lifecycle payloads.

Claude batch events create one marker rather than duplicate individual tool
closes. Content-bearing error/compaction/batch values are hashed. API failures
retain safe error categories; tool diagnostics never become raw status text.
Codex shell output has no universal success flag, so opaque tools remain unset;
typed MCP `content` plus `isError` permits explicit status. This avoids guessing
success from arbitrary output.
