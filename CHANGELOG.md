# Changelog

Notable changes are recorded here using [Keep a Changelog](https://keepachangelog.com/).
Gently follows [Semantic Versioning](https://semver.org/) from 1.0.0; pending
changes appear under Unreleased.

## [Unreleased]

## [1.0.0] - 2026-10-09

First release: local-first observational tracing for Claude Code and Codex in
their CLI and desktop coding surfaces, with metadata-only capture by default,
opt-in encrypted raw values and a self-hosted Cloudflare collector. Release
archives cover macOS on Apple silicon and x86_64 Linux. See
[release verification](docs/releases/1.0.0/verification.md) for the evidence.

### Security

- Update rustls to 0.23.45 (RUSTSEC-2026-0285, TLS 1.3 handshake messages
  accepted across encryption levels), quinn-proto to 0.11.15 (RUSTSEC-2026-0185,
  memory exhaustion from out-of-order stream reassembly) and anyhow to 1.0.103
  (RUSTSEC-2026-0190, unsound `Error::downcast_mut`).
- Audit the locked Rust dependencies in CI with a pinned cargo-audit. Every
  RustSec vulnerability or warning fails unless a reviewed, expiring exception
  records it.

### Added

- Add `scripts/native-evidence.py`, which runs one real Claude Code or Codex session
  (CLI or desktop) against isolated state, an enrolled reader and an ephemeral
  local collector, and verifies native receipts, the agent's own MCP query,
  encrypted export, reader decryption and canary absence for release evidence.
- Add an owner-dispatched release workflow for `v*` tags. It builds locked macOS
  arm64 and Linux x86_64 binaries without local source paths, packages
  deterministic archives with `SHA256SUMS` and GitHub build provenance, then
  verifies provenance, checksum, layout, an in-place upgrade from the previous
  version, backup/restore, uninstall and the full CLI/Worker acceptance against
  the released binary. The readiness policy allows it only attestation writes, and
  publication stays a manual owner step.
- Add `gently export --discard-oldest`, the only way to trim queued history to
  `outbox_cap`; it drops the oldest envelopes before each drain, even if delivery fails.
- Add `gently quarantine list`, which prints bounded, payload-free JSON summaries
  of metadata quarantine with a typed category and the collector's HTTP status,
  and `gently quarantine retry --id ID`, which atomically requeues one envelope.
- Add tenant-bound tokenless local query delegation and backlog-preserving launchers.
- Require explicit public-repository review through commit, message and push hooks;
  inspect stored Git objects despite replacement refs and reject captured telemetry,
  reader identities, ciphertext, database/archive contents and local home paths.
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

- Preserve every queued envelope by default: `gently export` no longer trims to
  `outbox_cap`. `--preserve-backlog` is hidden but still accepted. Downgrading to
  an earlier binary brings back default trimming unless `--preserve-backlog` is
  passed, so keep it in launchers until earlier binaries are retired; the bundled
  hook and local launchers still pass it.
- Refresh all 31 observational Claude Code and 12 Codex command-hook registrations,
  including desktop coding runtimes; safely migrate covered legacy Codex JSON handlers.
- Retain immutable event receipts and useful display, instruction, model/cache metadata;
  encrypt complete hook payloads under the existing recipient policy when capture is enabled.
- Preserve provisional tool visibility, invocation IDs and runtime durations; keep tool
  rollups accurate across replay, unfinished invocations and permission observations.
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

- Protect local collector files with owner-only permissions and disable the
  local Worker's DevTools inspector; bound startup and cleanup after runtime failures.
- Retry query-broker accept failures without stopping export and report its concurrency cap.
- Skip optional linked or foreign-owned Codex JSON migration without changing user files.
- Explain staged whitespace and unavailable remote-history sanity failures without
  printing Git output or weakening publication checks.
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
