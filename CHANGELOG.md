# Changelog

All notable changes to gently are recorded here. Format follows
[Keep a Changelog](https://keepachangelog.com/); this project is pre-1.0 and not
yet versioned, so changes accrue under **Unreleased**.

## [Unreleased]

Hardening of the hook path and collector.

### Added

- **Richer span attributes**, captured when the harness payload carries them:
  `gently.model` (every Codex event), `gently.source` (SessionStart),
  `gently.effort`, `gently.reason` (SessionEnd), and
  `gently.agent_transcript_path` (SubagentStop). Unblocks model/cost breakdowns
  straight from the collector. Claude payloads omit `model`, so it is a no-op
  there.
- **Effective span bounds.** `get_trace` now returns derived
  `effective_start_unix_nano` / `effective_end_unix_nano` - the envelope of a
  span and its subtree - so a parent whose own hook events don't bound its
  children (an unclosed Codex session root; an auto-turn) renders its true window.
  O(N), no recursion. `waterfall.py` and the MCP `get_trace` passthrough consume
  the effective pair; raw bounds are kept for integrity checks.
- **Inferred turns.** The applier back-fills a provisional turn span (marked
  `gently.event = "TurnInferred"`) the first time *any* event references a turn,
  so tools under Codex auto-continuation turns (which fire no `UserPromptSubmit`)
  no longer dangle.
- **`open_spans` TTL reaper** - drops provisional rows older than a day so a span
  whose close never arrives (no Codex `SessionEnd`; Esc-interrupted turns fire no
  `Stop`) can't grow the local DB without bound.
- **Debug-capture facility** (`GENTLY_DEBUG`) now namespaces raw payloads by
  harness and writes an env snapshot, for adapter-coverage audits.

### Changed

- **Idempotent-monotonic ingest.** The collector replaces last-write-wins
  `INSERT OR REPLACE` with an order-independent upsert: a span's stored extent is
  the envelope of all its reports (`MIN(start)`, `MAX(end)`) and content comes
  from the most-finalized report. Replays and out-of-order delivery converge.
- **4xx classification.** Auth/throttle rejections (`401`/`403`/`408`/`429`) are
  now retryable; only genuinely unprocessable 4xx (`400`/`413`/`422`) is
  quarantined.
- Codex adapter docs refreshed to `0.139`, recording the structural hook gaps
  (no `SessionEnd`/`StopFailure`/`exit_code`) confirmed open upstream.

### Fixed

- **Auth rejections no longer dead-letter valid spans.**
- **Codex / interrupted-turn durations render correctly** via `effective_end` -
  and without recursion.
- **Dangling tool spans** under Codex auto-turns - their never-created turn span is now back-filled.
- **Resumed session root.** Its start is preserved (monotonic ingest) instead of
  being shoved forward to the latest `SessionStart`, and its `effective_end`
  always spans the trace even when monotonic ingest leaves `end != start`.
