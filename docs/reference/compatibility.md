# Coding-agent compatibility

Checked on 5–6 October 2026 against the installed runtimes and current official
hook references. This records a compatibility baseline, not a claim that every
future release or desktop execution mode exposes the same events.

| Surface | Version checked | Integration |
| --- | --- | --- |
| Codex CLI | 0.160.0 | 12 command hook events; stdio MCP |
| Codex desktop coding runtime | 0.159.0 in desktop app 26.928.20755 | Same local hook/config contract; stdio MCP |
| Claude Code CLI | 2.1.285 | 31 observational command hook events; stdio MCP |
| Claude Desktop Code | Runtime 2.1.288 in app 2.19675.1; also checked 2.1.286 in 2.19675.0 | Claude Code hooks and user MCP configuration |
| Codex CLI latest release, isolated | 0.160.1 | 12 enabled/trusted command hooks; native tool capture and MCP query |
| Claude Code latest release, isolated | 2.1.291 | 31 observational command hooks; native tool capture and MCP query |

The isolated versions are the latest releases listed in the official changelogs
on 6 October. They were installed only in a temporary verification directory;
the user's CLI and desktop installations were left at the versions listed above.
Both native schemas exactly match Gently's event registrations. Claude's newer
mod/plugin hooks such as `turn.step` and `tool.check` are a separate programmatic
interface; this integration observes the documented command-hook contract.

Native hook inventory, tool capture and MCP query checks succeeded for all four
installed coding-agent surfaces and the two isolated latest CLI releases. These
checks used the compatibility build before integration onto the encrypted-store
baseline. The integrated PR is additionally validated with encrypted-capture,
tenant-isolation and local collector acceptance tests; the desktop clients are
not upgraded or reinstalled by these tests. Already-running clients can retain
old hook sources until restarted. Claude Chat and Cowork are outside scope.

## Current hook formats and useful capture

Here, fidelity means keeping up with the coding runtimes' current command-hook
formats and event vocabulary, and retaining useful observations for queries.
It includes lifecycle correlation, message batch progress, runtime duration,
failure categories, context changes and available cache estimates.

Every successfully processed hook gets a separate immutable `hook:<Event>`
receipt, including tool opens, resumes and events whose fields Gently does not
yet model. Its byte length describes the complete normalized JSON payload. No public
content fingerprint is recorded. Receipts parent to the session or executing subagent and use separate
IDs, so later session/turn/tool updates do not overwrite earlier receipts.
JSON numbers retain arbitrary precision, including integers beyond 64 bits,
long decimal values and valid exponents beyond floating-point range.
Receipt tool identifiers use `gently.hook.*`, keeping receipts out of tool
duration rollups. Aggregate spans retain the existing lifecycle and status rules.
Parsed metadata is also retained on each receipt, so later aggregate updates do
not erase earlier observations such as a resume's cache estimate.

Claude message observations retain the display message ID, its supplied turn ID,
batch index, final flag and the byte length of the delta. Empty final deltas still mark
message completion. Instruction-load observations retain the documented memory
scope and load reason, with the byte length of the file path. Session resumes and model
switches retain typed context-token/cache metadata and the supplied cache-write
cost estimate; these are runtime estimates, not measured usage or billed costs.

With `capture_raw_values = true` and an approved signed recipient manifest,
complete normalized JSON payloads are encrypted before persistence in private
local SQLite, including nested values, failures, compaction, batches and unknown
fields. Capture requires public recipient policy and no private reader key.
Optional ciphertext synchronization remains a separate opt-in. With explicit
reader resolution and a configured enrolled identity, `get_trace` and
`search_spans` can decrypt referenced payloads in memory into the
`gently.hook_payload` attribute. Capture, sync and resolution remain off by
default. Raw payloads never enter ordinary OTLP exports. See
[security and privacy](../concepts/security-and-privacy.md).

Hook fidelity covers what the runtime sends to a configured, trusted hook.
It cannot recover absent events, hidden reasoning, every intermediate assistant
message, unsupported tools, binary attachments or a complete transcript from
metadata alone. Gently records supplied transcript paths but does not scrape
transcripts or native authentication stores. Missing IDs can limit parentage;
storage failures, queue trimming and absent closing hooks can limit delivery.

Claude's `WorktreeCreate` and `WorktreeRemove` hooks replace worktree operations.
Gently leaves them unregistered because a silent telemetry hook would disrupt
creation/removal. User-owned worktree handlers can explicitly forward a payload
to `gently hook` while performing their own operation. `FileChanged` captures
events for files the runtime already watches; Gently does not broaden that list.

Codex reports opaque shell output without a universal tool-exit status. Gently
preserves unset status rather than guessing from text; typed MCP `isError`
results can establish success/failure. See [hook mappings](hooks.md).

## Desktop queries without credential copies

On Unix, start an unlocked
`gently export --watch --serve-queries --preserve-backlog`. The last option
keeps accumulated history instead of trimming it to the configured cap. The bundled
collector/export launchers enable this option. Tokenless CLI and MCP processes
use `<state_dir>/tenants/<tenant_id>/devices/<device_id>/query.sock`, provided
their state directory, tenant, device and collector URL match the watcher. The watcher holds the bearer token and forwards only
read-only metadata and encrypted-object downloads. The caller still needs its
explicit reader identity to decrypt ciphertext. No token is written to desktop configuration or
returned to the caller. Other platforms need an inherited query token.

On an incompatible pre-encryption local schema, preserve any needed private
history and follow the explicit reset instructions in the privacy guide. After
upgrading, reinstall Gently, rerun both init commands, restart the local
launcher and restart the coding clients. Trust any new Codex hook definitions
through its hook UI. Preserve existing raw-resolution preferences deliberately
when rerunning init; init without the raw flag removes that registration opt-in.
Codex init also removes exact legacy Gently handlers for this executable from
`~/.codex/hooks.json` when the inline registration covers their matcher. Codex
loads both sources, so leaving old default-Claude handlers alongside the Codex
registration can duplicate capture and mislabel the harness. Custom handlers,
unknown events and unmatched coverage remain untouched.

## Primary references

- [Codex hooks](https://learn.chatgpt.com/docs/hooks): local configuration,
  event input, tool coverage and trust requirements.
- [Claude Code hooks](https://code.claude.com/docs/en/hooks): event schemas,
  terminal/desktop behavior and worktree replacement semantics.
- [Codex changelog](https://learn.chatgpt.com/docs/changelog) and
  [Claude Code changelog](https://code.claude.com/docs/en/changelog): latest
  release targets checked separately from installed versions.
