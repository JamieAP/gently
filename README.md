<div align="center">

# gently

**Distributed tracing for coding agents.**

Turn the lifecycle of a Claude Code session - every prompt, tool call, and subagent -
into OpenTelemetry spans, ship them over QUIC to a Cloudflare edge worker, and query
your own traces from the CLI or from *inside the agent itself* over MCP.

</div>

---

A coding agent is a distributed system you can't see into: prompts fan out to tools,
tools spawn subagents, work happens across processes that live for milliseconds.
gently makes that legible. It hooks the harness, reconstructs the causal tree, and
gives you a waterfall of what the agent actually did - and how long it took.

```
  Claude Code
      │  hooks (every tool call)
      ▼
  gently hook ──► ~/.gently/state.db        local state write,
      │            (WAL outbox, durable)     never touches the network
      │
      └─ spawns ─► gently export ──QUIC/HTTP3──► gently-collector (Worker) ──► D1
                     (detached)    h2 fallback        bearer auth          (SQLite)
                                                            ▲
   gently traces│trace│spans│stats ───── GET /v1/query ─────┤
   gently mcp  (stdio MCP, exposed to the agent) ───────────┘
```

v1 wires **Claude Code**. The hook layer is a `Harness` trait with a `ClaudeCode`
adapter, so Codex and Cursor slot in as new adapters without touching the core.

---

## Design

The interesting decisions, and why:

- **Deterministic ids.** `trace_id = blake3(session_id)`, `span_id =
  blake3(session_id, key)`. A child computes its parent's id without the parent
  existing yet and without any local state - so the tree reconstructs across
  separate, short-lived hook processes, and ingest is idempotent.
- **Self-contained spans.** No span depends on a parent being present. A session
  that dies without `SessionEnd` still renders; the backend assembles the tree from
  `(trace_id, parent_span_id)` alone. Sessions, turns and subagents emit a
  *provisional* span on open so an interrupted one survives a crash, finalized on
  close via idempotent replace.
- **The hot path is sacred.** `gently hook` runs on *every* tool call. It writes one
  row to a local SQLite WAL and spawns the exporter detached - no network,
  no QUIC client, no blocking, always exit 0, never a byte on stdout (which the
  harness would parse as control output). 
- **Durable & self-healing.** Completed spans queue in a local outbox; a `flock`'d,
  detached exporter drains it over QUIC. A down collector just means the queue grows
  and the next run retries. Idempotent ingest makes retries free.

## Quick start

**1 - Deploy the collector** (Cloudflare account required):

```bash
cd worker
npm install
wrangler d1 create gently                 # paste database_id into wrangler.toml
wrangler d1 execute gently --remote --file schema.sql
wrangler secret put GENTLY_TOKEN          # a shared bearer token
wrangler deploy                           # → https://gently-collector.<acct>.workers.dev
```

**2 - Install the agent integration:**

```bash
cargo install --path crates/gently-cli    # installs `gently`
gently init --claude                       # hooks + MCP server + ~/.gently/config.toml
```

Set your collector in `~/.gently/config.toml` (or `GENTLY_COLLECTOR_URL` /
`GENTLY_TOKEN`), then restart your session. Spans flow on every tool call.

**3 - Query** - from the shell, or as MCP tools the agent can call on itself:

```bash
gently traces                 # recent sessions
gently trace <id>             # the span tree
gently spans --tool-name Bash # filter
gently stats                  # per-tool rollups
gently trace <id> --json | python3 scripts/waterfall.py   # render a waterfall
```

MCP tools: `list_traces`, `get_trace`, `search_spans`, `trace_stats`.

## The trace model

OTLP/JSON over HTTP/3 (HTTP/2 fallback). One trace per session; spans named
`session`, `turn:N`, the tool name (`Bash`/`Read`/…), or `agent:<id>`.

| layer | carries |
|---|---|
| **resource** (per session) | `service.name`, `gently.harness`, `gently.session_id`, `gently.cwd`, `host.name`, `os.type`, `gently.version` |
| **span** | deterministic `traceId`/`spanId`/`parentSpanId`, `name`, `kind` (Internal/Client), `start`/`endTimeUnixNano` (string-encoded), `status` |
| **span attrs** (`gently.*`) | `event`, `tool_name`, `tool_use_id`, `permission_mode`, and `…sha256` + `…bytes` digests of input/response/prompt |

The Worker flattens these into a D1 `spans` table (trace-scoped attrs lifted from the
resource), keyed on `span_id` with `INSERT OR REPLACE` for idempotency.

### OpenTelemetry: compliant wire, two deliberate deviations

The bytes are valid OTLP/JSON. But two patterns are intentionally non-idiomatic and
work only because gently owns its collector - **don't point the exporter at a generic
backend (Tempo/Jaeger/Honeycomb) without accounting for them**:

1. **Deterministic ids** where OTel recommends random - the price of stateless
   reconstruction and idempotency.
2. **Provisional-then-final double emit** of the same `span_id`, where standard OTel
   emits each span once at end. It relies on the collector doing last-write-wins by
   `span_id`; a backend that doesn't upsert would show duplicates. The payoff is
   crash-durable in-flight spans, which the stable OTel model doesn't offer.

To target a standard backend: emit on close only (losing crash durability), or ensure
the backend dedups by `span_id`.

## Layout

| crate / dir | responsibility |
|---|---|
| `gently-core` | deterministic ids, span type, OTLP/JSON encoding (no I/O) |
| `gently-store` | `~/.gently/state.db`: open-span tracking + outbox (WAL) |
| `gently-harness` | `Harness` trait + `ClaudeCode` adapter + the stateful applier |
| `gently-export` | `Transport` trait, QUIC (reqwest-http3) + HTTP/2, outbox drain |
| `gently-cli` | the `gently` binary: `hook` · `export` · `traces`/`trace`/`spans`/`stats` · `mcp` · `init` |
| `worker/` | `gently-collector` Cloudflare Worker (TypeScript, D1) |
| `scripts/waterfall.py` | ASCII waterfall + structural-integrity checker |

## Development

```bash
cargo test                                 # all crates
cargo clippy --all-targets -- -D warnings
cd worker && npm test                      # Worker (vitest + Miniflare)
```

The Claude Code hook schema is verified empirically, not assumed: run with
`GENTLY_DEBUG=1` and inspect `~/.gently/raw/<Event>.jsonl`. The adapter is tolerant -
only `session_id` and `hook_event_name` are required; any unmodeled event becomes a
marker span.

---

<div align="center"><sub>The best Rust reads like Erlang. Components die; the trace survives.</sub></div>
