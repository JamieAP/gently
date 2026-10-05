# Architecture

Gently separates local capture from delivery and querying. Hooks write locally;
an exporter sends queued records to the collector; CLI and MCP queries read the
collector's stored traces.

```text
Claude Code / Codex hooks
           |
       gently hook
           |
   local SQLite state.db <--- gently status
           |
   gently export [--watch]
           | OTLP/JSON + bearer token
           v
      Worker -> D1
           ^
           | collector query API
    query commands / MCP
```

## Components

| Component | Responsibility |
| --- | --- |
| `gently hook` | Parse one hook payload, update span bookkeeping and queue the emitted spans. |
| Local `state.db` | Hold unsent envelopes, open spans, turn counters, quarantine and optional raw values. |
| `gently export` | Drain the outbox once, or keep polling with `--watch`. A file lock allows one exporter at a time. |
| Worker and D1 | Accept OTLP/JSON reports, merge repeated span IDs and store traces. The Worker also serves queries. |
| CLI and MCP | Query the collector. `gently status` reads local exporter health; `gently waterfall` renders supplied JSON offline. |

The default local database is `~/.gently/state.db`. The collector uses D1 in
both local Wrangler development and Cloudflare deployments. These databases
have different roles; the local database is not a second trace-history server.

## From hook to collector

1. A configured harness starts `gently hook` with an event's JSON on stdin.
2. The adapter extracts identifiers, metadata and content digests. The applier
   updates local open-span records and counters.
3. Emitted spans are queued together in one OTLP envelope. An event can emit
   more than one span, such as an inferred turn and a completed tool.
4. If both collector URL and token are available, the hook may start a detached
   exporter. Otherwise the envelope remains queued for a watcher or later drain.

The hook process does not make collector requests or unlock credentials. Its
errors and caught panics are contained, and it writes no normal stdout. A
successful hook exit does not prove that capture or export succeeded: parsing,
local storage, process startup or delivery can fail. See
[Reliability](reliability.md) for those boundaries and health checks.

Export runs separately. Remote delivery can prefer HTTP/3 with a TCP fallback;
local HTTP uses the TCP path. Query commands use the Worker's query API rather
than a general OpenTelemetry backend interface.

## Storage and query boundaries

Successful delivery removes acknowledged outbox envelopes. It does not remove
local raw values, quarantine, logs or other bookkeeping. Collector queries read
D1, so queued spans do not appear in trace history until delivered.

Raw capture and local query resolution are separate opt-ins. Resolution can
look up retained raw values in local SQLite and enrich a collector result; it
does not make SQLite a trace-history query source. Read
[Security and privacy](security-and-privacy.md) before enabling either option.

A token-bearing watcher can deliver records queued by tokenless desktop hooks.
Its token belongs to that process; starting it does not authenticate separate
CLI or MCP query processes. The optional local launchers invoke a separately
installed secret helper, as described in
[Local collector setup](../getting-started/local-collector.md).

For span lifecycles, see [Trace model](trace-model.md). For endpoint and storage
details, see [Collector reference](../reference/worker.md).
