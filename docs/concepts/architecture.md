# Architecture

gently is a four-stage pipeline plus a query path. The two SQLite databases are
not alternatives - they're different jobs.

```
  Claude Code / Codex ──hook JSON──► gently hook
                                     │ 1. parse event
                                     │ 2. update local state.db (WAL)
                                     │ 3. enqueue one event envelope → outbox
                                     │ 4. spawn exporter if token is available
                                     │ 5. exit 0, empty stdout
                                     ▼
                              ~/.gently/state.db (outbox + open spans + counters)
                                     │
       gently export [--watch] ◄─────┘  (flock singleton; drains outbox)
                   │ OTLP/JSON, prefer HTTP/3 (QUIC), HTTP/2 fallback, bearer auth
                   ▼
        gently-collector Worker ──► D1 (spans table)
                   ▲                      │
   gently traces/trace/spans/stats/status, gently mcp ── GET /v1/query ──┘
```

## Components

| Stage | What it does |
|---|---|
| **hook** (`gently hook`) | The harness invokes this on every event. Local: parse → write one event envelope → optionally spawn exporter → exit. No network or secret unlock on the hook path. |
| **outbox** (`state.db`) | A local SQLite WAL database: the durable staging buffer plus in-flight open spans and per-session counters. |
| **exporter** (`gently export`) | Drains the outbox over QUIC/HTTP; one-shot by default, or polls with `--watch`. |
| **collector** (Worker + D1) | Receives OTLP/JSON, persists spans to D1 (idempotent), and serves queries. |
| **query** (CLI + MCP) | Reads the collector's `/v1/query` surface. |

## The hot path

`gently hook` runs on *every* tool call, so it must be invisible:

* Queues all spans emitted by one event in a single SQLite outbox row.
* Builds **no** network client and opens **no** connection.
* When a token is available, spawns the exporter detached (new process group,
  stdio to `/dev/null`) and returns immediately. Tokenless hooks only queue.
* **Never writes stdout** (the harness parses hook stdout as control output) and
  **always exits 0** - a panic is caught and logged, never surfaced.

Network and retry costs live in the exporter. Hardware-bound token unlock
requires a foreground terminal; an already-unlocked watcher can export for
desktop hooks that lack a token.

## Two databases, two jobs

* **Local SQLite (`state.db`)** - write-side staging only. Spans land here first,
  then ship to the collector and are **deleted** from the outbox on success. It
  is never queried for trace history.
* **Cloudflare D1** - the durable store of record *and* the only query source.

Raw-value capture is disabled by default and separately configurable from query
resolution; see [Security & privacy](security-and-privacy.md).

See [Reliability](reliability.md) for queue and watcher behavior, and
[Trace model](trace-model.md) for span structure.
