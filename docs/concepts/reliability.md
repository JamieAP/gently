# Reliability

gently is reliable *as just a hook* - there is no daemon, no scheduler, nothing
long-lived to install or babysit. Each `gently export` is a disposable worker;
the next hook is its supervisor. The durable outbox is the source of truth.

## What triggers export

Export is **event-driven**, not timer-driven. After a hook enqueues its spans it
spawns a detached `gently export`, subject to one throttle:

* **Terminal events** (`Stop`, `StopFailure`, `SessionEnd`) → **always** spawn, so
  a turn/session's final spans always flush.
* **All other events** → spawn only if no exporter is already running (a
  non-blocking `flock` probe). A running exporter drains in a loop and picks up
  the freshly-enqueued row.

A single exporter drains until the outbox is empty. `flock` makes it a singleton:
redundant spawns exit immediately. Cross-session, the next session's
`SessionStart` (itself a hook) drains anything a previous crashed/offline session
left behind.

## Failure handling

The drain classifies every failure:

* **Retryable** - collector unreachable, timeout, 5xx, **or a recoverable 4xx
  (`401`/`403`/`408`/`429`)**. Auth and rate-limit rejections aren't the payload's
  fault: a stale token or a throttle clears, so the spans stay queued and drain
  once it does. The run retries in-place with **exponential backoff** (250 ms →
  500 → 1 s); beyond that it exits and the next hook retries. Nothing is lost.
  
* **Rejected (poison)** - a genuinely unprocessable 4xx (`400`/`413`/`422`): the
  same bytes will never succeed. The drain **bisects** the batch to isolate the
  offending span and moves it to a `quarantine` table, so one poison span can't
  wedge the queue. Good spans in the same batch are still delivered.

### Idempotent, order-independent ingest

The hook layer is a distributed, retrying, multi-process emitter with no
delivery-order guarantee - the same `span_id` is reported provisionally on open,
again on close, and (for the session root) again on every resume. The collector
ingests **monotonically**: a span's stored extent is the *envelope* of all its
reports (`MIN(start)`, `MAX(end)`) and its content comes from the most-finalized
report (the one that ends latest). This is commutative - replays and out-of-order
delivery converge to the same row - so a retried provisional can never revert a
finalized span, and a resume can never push the session root's start past its own
history.

## Durability & crash behavior

* Completed spans live in the durable WAL outbox until a 2xx, then are deleted.
* A killed exporter loses nothing - rows aren't removed until acknowledged.
* In-flight (open) spans persist in `state.db`; provisional emit means an
  interrupted session/turn/subagent still appears (see
  [Trace model](trace-model.md)). A span whose close never fires (Codex has no
  `SessionEnd`; an Esc-interrupted turn fires no `Stop`) would otherwise linger,
  so a **TTL reaper** drops local `open_spans` rows older than a day on each hook
  - purely local bookkeeping; the provisional span already lives in the collector.
* WAL + `busy_timeout` lets many concurrent hook processes (parallel tools,
  multiple sessions) write without lock errors.

## Observability - `gently status`

The detached exporter records its outcome in a `health` row, surfaced by
`gently status`, so a silently-failing exporter is visible by *querying*:

```
| pending (outbox)     | 0       |
| quarantined          | 0       |
| consecutive_failures | 0       |
| last_export          | 1s ago  |
| last_success         | 1s ago  |
| last_error           | -       |
```

Logs (`export.log`, `hook.log`) rotate at 5 MB.

## Known trade-offs

* **No connection reuse** - each exporter is a fresh process, so it pays a
  TLS/QUIC handshake per run. This is off the hot path (the hook doesn't wait), so
  it's wasted CPU in a detached process, not user-visible latency. Eliminating it
  would require a long-lived connection (a daemon), which the design deliberately
  avoids.
* **Idle outbox** waits for the next hook to drain - acceptable, since an idle
  agent has nothing new to report and the next activity (or next `SessionStart`)
  flushes it.
