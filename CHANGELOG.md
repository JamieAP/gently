# Changelog

Notable changes are recorded here using [Keep a Changelog](https://keepachangelog.com/).
The project is pre-1.0; pending changes appear under Unreleased.

## [Unreleased]

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

- Align the waterfall separator with the header and span timeline borders.
- Keep valid spans eligible for retry after authentication rejection.
- Calculate trace bounds without recursive subtree queries.
- Link tool spans to inferred turns during Codex continuation.
- Preserve a resumed session's original start and derive its full trace window.
- Scope turn and tool tracking to execution agents while retaining the root
  session trace and nesting lifecycle spans under their parent tools.
- Quarantine malformed outbox JSON without blocking subsequent valid envelopes.
