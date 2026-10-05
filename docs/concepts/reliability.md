# Reliability

The durable SQLite outbox is the source of truth for unsent spans. Hooks are
short-lived local writers; exports run in disposable workers or an explicitly
started polling watcher.

## What triggers export

Each event queues one OTLP envelope containing all spans it emitted. With a
collector token available, a hook starts detached `gently export`:

* Terminal events (`Stop`, Claude `StopFailure`, Codex `Interrupt`, and
  `SessionEnd`) request a final flush.
* Other events skip spawning when the exporter singleton lock is already held.

Without a token, hooks queue without spawning an exporter. They never attempt
interactive hardware unlock. `gently export --watch --interval-secs 2` polls the
queue independently, so desktop hooks can be drained by a process whose token
was unlocked once in a foreground terminal. Stop it with Ctrl+C; the local
collector launcher starts this watcher alongside the Worker.

A `flock` lock prevents concurrent drains. A one-shot export drains until empty;
a watcher keeps polling for new rows. Queues from older versions containing
single-span rows remain compatible with multi-span event envelopes.

## Failure handling

* Authentication (`401`/`403`) stops immediately. The queue stays intact: no
  retries with the same token, HTTP fallback, or quarantine. Correct the token
  and restart export or the watcher.
* Network failures, timeouts, 5xx and non-payload endpoint errors such as
  `404`/`405`/`409`/`408`/`429` use exponential backoff. Failed rows remain queued
  for a later drain.
* Unprocessable requests (`400`/`413`/`422`) are bisected by outbox row. Rejected
  envelopes move to quarantine so other rows can be delivered. A quarantined
  event envelope can contain several spans; those spans are retained together,
  including valid sibling spans. Malformed queued JSON is quarantined with a
  safe reason.

Aggregate export batches split at 4 MiB; a larger single envelope is sent alone
for the collector to accept or reject. The outbox capacity counts envelopes. A prolonged outage beyond the configured
cap drops the oldest queued rows; status and diagnostics make failures visible.

## Idempotent, order-independent ingest

A deterministic span ID is reported provisionally on open and again on close.
Session resumes preserve the earliest local open-session start. The collector
merges reports monotonically: earliest start, latest end, and the most-finalized
content. Replays and out-of-order delivery therefore converge without replacing
a finalized span with an older provisional report.

## Durability and crash behavior

* Outbox rows remain in WAL storage until a successful acknowledgement.
* A killed exporter leaves unacknowledged rows available for another drain.
* Session, turn and subagent spans are emitted provisionally, so an omitted close
  still anchors the tree. Current Codex includes `SessionEnd` and `Interrupt`;
  crashes or older harnesses can still omit lifecycle hooks. A one-day TTL reaper
  drops stale local open-span bookkeeping; it does not remove emitted spans.
* WAL and `busy_timeout` support concurrent tools and sessions. Prompt/turn IDs
  and execution-agent context keep their spans correctly associated.

## Observability

`gently status` reports pending outbox rows, quarantine count, consecutive
failures, export/success timestamps and the latest error. `export.log` and
`hook.log` rotate at 5 MB. A polling watcher uses the same health reporting.

A one-shot exporter pays process and connection startup on each run. An idle
queue needs a later token-bearing hook, manual export or watcher to flush it.
The watcher requires an explicit start and does not act as a login service.
