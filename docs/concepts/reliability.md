# Reliability

Gently queues emitted records in a local SQLite outbox and retries delivery
separately from the hook. This can preserve records through a collector outage
or exporter restart. It does not guarantee that every harness event is observed,
queued or eventually delivered.

## Starting a drain

A hook with a collector URL and token may start a detached one-shot exporter.
Terminal events request a final drain; other events skip startup when an
exporter lock is already held. Tokenless hooks only queue records and never
request interactive credential unlock. Hook-spawned exporters pass
`--preserve-backlog` to avoid deleting accumulated history.

A one-shot exporter drains until the queue is empty or a failure stops the run.
It exits if another exporter already holds the file lock. A watcher retains the
lock and polls for new rows:

```sh
gently export --watch --interval-secs 2
```

The watcher needs a token in its inherited environment.
Gently does not unlock a secret store itself. A provider can wrap the
[local launchers](../getting-started/local-collector.md) after their dependency
preflight. A watcher's token does not authenticate other CLI or MCP
processes, and the watcher is not installed as a login service.

Stopping a watcher with Ctrl+C ends that drain loop. Start it again after a
restart. Without a watcher, a later token-bearing hook or manual export is
needed to flush an idle queue.

## Delivery failures

| Failure | Export behavior | Next step |
| --- | --- | --- |
| Authentication, `401` or `403` | Stop the run without retrying, falling back or quarantining the rejected send. Pending rows remain, subject to the capacity trimming below. | Correct the token and restart export or the watcher. |
| Network error, timeout, 5xx or other endpoint rejection such as `404`, `405`, `408` or `429` | Back off; undelivered rows remain queued. | Check the endpoint and connectivity, then retry. |
| Payload rejection, `400`, `409`, `413` or `422` | Split batches to isolate rejected envelopes and retain them in quarantine. | Inspect the rejected envelope and collector limits. |
| Malformed queued JSON | Move that envelope to quarantine with a fixed diagnostic reason. | Inspect local storage and the producing version. |

One-shot export makes up to three attempts for retryable failures. A watcher
continues retrying with backoff, capped at 30 seconds. Authentication errors
stop either mode; later hooks may start a new exporter with the same still
invalid token, so repeated failures require fixing configuration.

The collector limits requests to 1 MiB and 32 span reports. Oversized metadata is rejected and
handled through the payload rejection path.
An event envelope can contain several spans: quarantine operates on the whole
envelope, so valid siblings can remain with a rejected span. Quarantine retains
the bytes; moving a row there does not mean it reached the collector.

## Capacity and retention

`outbox_cap` defaults to 10,000 envelope rows. At the start of each drain, rows
beyond that cap are trimmed oldest first. Enqueueing does not enforce the cap,
so a tokenless or idle queue can grow beyond it until a drain starts. The cap
counts envelopes, not spans or bytes, and is not a bound on total database size.

Capacity trimming happens before a request, including one that later fails
authentication. Trimmed rows are lost. Each tenant/device database admits at
most 64 MiB of encoded ciphertext envelopes; SQLite page/WAL overhead is extra.
At capacity, capture retains metadata without new raw objects, and readers can
decrypt cloud objects in memory while skipping cache insertion. Existing and
pending ciphertext is never evicted automatically. Raw-value storage,
quarantine and collector traces have no automatic retention policy; cloud
quotas and scheduled deletion require a separate deployment policy.

## Replays and lifecycle gaps

Acknowledged envelopes are removed from the outbox. If the exporter stops
after the collector accepted a request but before local deletion, it may send
those reports again. The collector merges repeated span IDs rather than
creating duplicate rows within the tenant. A different capture device cannot
update a span owned by another host, and even its owner cannot change the trace
identity. Cross-device parent links are allowed.

Lifecycle merging keeps the earliest reported start and latest endpoint. For
tools, completed reports outrank provisional opens; completed reports with a
runtime `duration_ms` outrank timings inferred from hook timestamps and retain
their start and end together. Among reports of equal precedence, status,
attributes and parent metadata come from the latest ending timestamp; equal
timestamps favor the newly received report. Conflicting metadata with tied
timestamps can therefore depend on delivery order.

SQLite WAL and a busy timeout support concurrent hook writers. Each hook commits
its lifecycle changes, opt-in encrypted raw object and outbox envelope in one immediate
transaction. A failed write or caught panic rolls back those SQLite changes,
preserving prior open-span state and inferred-turn bookkeeping for a retry.
Database and sidecar permission checks preserve SQLite's POSIX locks: opening
and closing an unrelated ordinary descriptor for the same file can release
those locks, as described in [SQLite's locking guidance](https://www.sqlite.org/howtocorrupt.html).
Linux permission changes use an `O_PATH` descriptor through procfs; systems
without procfs must provide files already restricted to owner-only access.
Hooks contain ordinary
errors and caught panics, so exit status alone cannot establish that a record was
stored. Process termination, storage faults, missing hook events and capacity
trimming can still cause incomplete traces; Gently cannot replay an event the
harness does not resend.

Session, turn, tool and subagent opens emit provisional reports. Missing closes
leave provisional records with unset status, alongside independent `hook:<Event>`
receipts of the received events. During later hook
processing, local open-span bookkeeping with a start more than 24 hours old is
reaped. This does not remove emitted collector records, but a very long-lived
span can lose the local timing state needed for a later close.

## Checking health

`gently status` reads local pending and quarantine counts, retained/pending raw
object and encoded-byte counts, failure count,
export/success timestamps and the latest error. It does not query the collector
or confirm that a particular trace is complete. Compare it with collector
queries when verifying delivery.

`hook.log` and `export.log` live in the tenant/device runtime directory. On process startup,
a log larger than 5 MiB is rotated to a single `.1` backup. Rotation is not a
continuous size limit, so a long-running watcher can exceed that threshold.
See [Troubleshooting](../guides/troubleshooting.md) for operational
checks, and [Security and privacy](security-and-privacy.md) before sharing logs
or local state.

## Ciphertext delivery

With `sync_raw_values` enabled, the exporter uploads immutable encrypted
objects without a reader key. Pending ciphertext retries retain the same
reference and bytes; acknowledgment marks only that tenant's successful
uploads. With sync enabled, pending ciphertext is attempted before draining
metadata, and an upload failure can stop the drain. A reference may still be
unavailable if sync is disabled or delivery is incomplete. No decryption is needed for queueing, transport or
cloud storage. Capture failures skip raw retention while metadata continues;
invalid recipient policy never causes a plaintext fallback.
