# Changelog

Notable changes are recorded here using [Keep a Changelog](https://keepachangelog.com/).
The project is pre-1.0; pending changes appear under Unreleased.

## [Unreleased]

- Refresh all 31 observational Claude Code and 12 Codex command-hook registrations,
  including desktop coding runtimes; safely migrate covered legacy Codex JSON handlers.
- Retain immutable event receipts and useful display, instruction, model/cache metadata;
  encrypt complete hook payloads under the existing recipient policy when capture is enabled.
- Preserve provisional tool visibility, invocation IDs and runtime durations; keep tool
  rollups accurate across replay, unfinished invocations and permission observations.
- Add tenant-bound tokenless local query delegation and backlog-preserving launchers.
- Preserve SQLite locks during permission hardening and exact JSON numbers on hook input.
- Require explicit public-repository review through commit/push hooks and inspect stored
  Git objects even when replacement refs are installed.

### Added

- Encrypt raw event fields before local persistence with age and random opaque
  references; cloud sync and explicit reader resolution are separate opt-ins.
- Sign tenant reader manifests, pin owner roots, minimum epochs and exact policy digests locally,
  enforce policy expiry, and manage encrypted reader/owner keys through private
  terminal commands. Support native age recipients across Macs and Linux.
- Store bounded immutable ciphertext in D1 and authorize each host for a tenant,
  device and ingest/read capabilities. Preserve authoritative span/trace ownership
  and bound metadata ingestion to 32 reports per request.

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
- Add a persistent export watcher and foreground local service supervision.
- Capture the current Claude Code and Codex hook events, including session end,
  interruption, compaction, and execution-agent context.

### Changed

- Replace secret-helper coupling with provider-neutral local launchers and
  credential-free preflight; keep optional foreground Mac provider wrappers.
- Require environment-only client credentials and a per-host Worker secret map.
- Remove public raw-content fingerprints and plaintext debug capture.
- Replace development state with a fresh encrypted-only schema and isolate local
  runtime state by tenant/device; no legacy aliases, readers or migrations.

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

- Preserve large integers and precise decimal JSON values in hook payloads;
  valid extreme exponents no longer cause the entire hook to be discarded.
  Keep user object keys literal in hooks, configuration rewrites and local queries.
- Preserve SQLite's POSIX locks during private database and sidecar permission
  checks, preventing concurrent writers from losing WAL shared-memory locks.
- Migrate covered legacy Codex JSON registrations to the canonical inline hooks,
  avoiding duplicate capture through the default Claude adapter while preserving
  user handlers and matcher coverage.
- Identify the exited local service and signal/status in launcher diagnostics,
  with conventional exit codes for signal termination.
- Align the waterfall separator with the header and span timeline borders.
- Keep valid spans eligible for retry after authentication rejection.
- Calculate trace bounds without recursive subtree queries.
- Link tool spans to inferred turns during Codex continuation.
- Emit one inferred parent for activity before the first observed prompt when
  turn IDs are absent, and propagate counter database errors instead of silently
  attaching activity to turn zero.
- Preserve a resumed session's original start and derive its full trace window.
- Scope turn and tool tracking to execution agents while retaining the root
  session trace and nesting lifecycle spans under their parent tools.
- Quarantine malformed outbox JSON without blocking subsequent valid envelopes.
