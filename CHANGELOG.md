# Changelog

Notable changes are recorded here using [Keep a Changelog](https://keepachangelog.com/).
The project is pre-1.0; pending changes appear under Unreleased.

## [Unreleased]

### Added

- Render trace waterfalls directly in the Rust CLI with `gently trace --waterfall`
  or `gently waterfall` for JSON on stdin. Include Unicode-aligned labels,
  clipping ellipses, a status legend and trace integrity diagnostics.
- Capture model, session source, effort, close reason, and agent transcript path
  when supplied by the agent's hook payload.
- Return effective bounds for trace rendering: a full-trace window for roots
  and direct-child bounds for other spans.
- Create provisional turn spans when events reference a turn before its opening
  event has arrived.
- Reap open-span tracking rows older than a day.
- Separate debug captures by agent; explicit raw capture and debug capture are
  both required, and environment snapshots are no longer collected.
- Add a persistent export watcher and local collector launchers that unlock a
  hardware-backed token once in a foreground terminal.
- Capture the current Claude Code and Codex hook events, including session end,
  interruption, compaction, and execution-agent context.

### Changed

- Replace the standalone Python waterfall renderer with the native commands.
- Reorganize setup, querying and reference documentation and add troubleshooting.
- Merge report time bounds and choose content by the latest report timestamp.
- Stop immediately on authentication rejection (`401`, `403`), preserving the
  queue for a later authenticated export. Retry transient failures with backoff
  and quarantine unprocessable envelopes (`400`, `413`, `422`).
- Queue one envelope per hook event and reuse export clients in watch mode;
  hooks without a token only queue locally.
- Disable local raw-value capture by default, with a separate opt-in for MCP
  resolution of captured values.
- Document current hook mappings and preserve unset tool status when Codex
  output supplies no typed success or failure signal.

### Fixed

- Keep valid spans eligible for retry after authentication rejection.
- Calculate trace bounds without recursive subtree queries.
- Link tool spans to inferred turns during Codex continuation.
- Preserve a resumed session's original start and derive its full trace window.
- Scope turn and tool tracking to execution agents while retaining the root
  session trace and nesting lifecycle spans under their parent tools.
- Quarantine malformed outbox JSON without blocking subsequent valid envelopes.
