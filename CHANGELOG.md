# Changelog

Notable changes are recorded here using [Keep a Changelog](https://keepachangelog.com/).
The project is pre-1.0; pending changes appear under Unreleased.

## [Unreleased]

### Added

- Capture model, session source, effort, close reason, and agent transcript path
  when supplied by the agent's hook payload.
- Return effective span bounds covering a span and its descendants for trace
  rendering.
- Create provisional turn spans when events reference a turn before its opening
  event has arrived.
- Reap open-span tracking rows older than a day.
- Separate debug captures by agent and filter environment snapshots by variable
  name.

### Changed

- Merge report time bounds and choose content by the latest report timestamp.
- Retry authentication and throttling responses (`401`, `403`, `408`, `429`),
  while isolating and quarantining unprocessable spans from rejected batches
  (`400`, `413`, `422`).
- Document Codex hook gaps where session-end, stop-failure, and exit-code events
  are unavailable.

### Fixed

- Keep valid spans eligible for retry after authentication rejection.
- Calculate trace bounds without recursive subtree queries.
- Link tool spans to inferred turns during Codex continuation.
- Preserve a resumed session's original start and derive its full trace window.
