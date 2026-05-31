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

* **Retryable** - collector unreachable, timeout, or 5xx. The run retries in-place
  with **exponential backoff** (250 ms → 500 → 1 s); beyond that it exits and the
  next hook retries. Rows stay queued; nothing is lost.
* **Rejected (4xx)** - the payload is bad and will never succeed. The drain
  **bisects** the batch to isolate the offending span and moves it to a
  `quarantine` table, so one poison span can't wedge the queue. Good spans in the
  same batch are still delivered.

Delivery is idempotent (`INSERT OR REPLACE` on `span_id`), so a retry that
actually succeeded upstream but lost the response is harmless.

## Durability & crash behavior

* Completed spans live in the durable WAL outbox until a 2xx, then are deleted.
* A killed exporter loses nothing - rows aren't removed until acknowledged.
* In-flight (open) spans persist in `state.db`; provisional emit means an
  interrupted session/turn/subagent still appears (see
  [Trace model](trace-model.md)).
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
