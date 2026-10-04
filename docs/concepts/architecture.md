# Architecture

gently is a four-stage pipeline plus a query path. The two SQLite databases are
not alternatives - they're different jobs.

```
  Claude Code ──hook stdin JSON──► gently hook        (hot path)
                                     │ 1. parse event
                                     │ 2. update local state.db (WAL)
                                     │ 3. enqueue completed spans → outbox
                                     │ 4. spawn detached `gently export`
                                     │ 5. exit 0, empty stdout
                                     ▼
                              ~/.gently/state.db (outbox + open spans + counters)
                                     │
              gently export ◄────────┘  (flock singleton; drains outbox)
                   │ OTLP/JSON, prefer HTTP/3 (QUIC), HTTP/2 fallback, bearer auth
                   ▼
        gently-collector Worker ──► D1 (spans table)
                   ▲                      │
   gently traces/trace/spans/stats/status, gently mcp ── GET /v1/query ──┘
```

## Components

| Stage | What it does |
|---|---|
| **hook** (`gently hook`) | The harness invokes this on every event. Pure-local: parse → write outbox → spawn exporter → exit. Never blocks, never touches the network. |
| **outbox** (`state.db`) | A local SQLite WAL database: the durable staging buffer plus in-flight open spans and per-session counters. |
| **exporter** (`gently export`) | A detached, disposable process that drains the outbox to the collector over QUIC and exits. |
| **collector** (Worker + D1) | Receives OTLP/JSON, persists spans to D1 (idempotent), and serves queries. |
| **query** (CLI + MCP) | Reads the collector's `/v1/query` surface. |

## The hot path

`gently hook` runs on *every* tool call, so it must be invisible:

* Writes one row to local SQLite (WAL), then returns without network work.
* Builds **no** network client and opens **no** connection.
* Spawns the exporter **detached** (new process group, stdio to `/dev/null`) and
  returns immediately.
* **Never writes stdout** (the harness parses hook stdout as control output) and
  **always exits 0** - a panic is caught and logged, never surfaced.

Network work lives in the detached exporter, off the hook's hot path.

## Two databases, two jobs

* **Local SQLite (`state.db`)** - write-side staging only. Spans land here first,
  then ship to the collector and are **deleted** from the outbox on success. It
  is never queried for trace history.
* **Cloudflare D1** - the durable store of record *and* the only query source.

See [Reliability](reliability.md) for how the exporter stays correct without a
daemon, and the [Trace model](trace-model.md) for span structure.
