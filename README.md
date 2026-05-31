# gently

Capture OpenTelemetry-compliant traces from agentic coding harnesses, ship them
to a Cloudflare edge Worker, persist them in Cloudflare D1, and query them from a
CLI that is also an MCP server the harness can call to introspect its own runs.

```
Claude Code ──hooks──► gently hook ──► ~/.gently/state.db (WAL outbox)
                                            │
                          gently export ────┘ ─OTLP/JSON over HTTP/2─► gently-collector (Worker) ─► D1
                                                                              ▲
   gently traces|trace|spans|stats  ───────────────────────────────────────┤  /v1/query
   gently mcp  (stdio MCP server, exposed to the harness) ──────────────────┘
```

v1 wires **Claude Code**. The hook layer is a `Harness` trait with a `ClaudeCode`
adapter, so Codex and Cursor drop in as additional adapters without touching the
core.

## Why it's built this way

- **Deterministic ids.** `trace_id = blake3(session_id)`, `span_id =
  blake3(session_id, logical_key)`. A child span computes its parent's id without
  the parent existing yet and without any surviving local state - so traces
  reconstruct correctly after a crash or a wiped state db.
- **Self-contained spans.** No span depends on a root existing. Sessions that die
  without `SessionEnd` still render a full trace; the backend assembles the tree
  purely from `(trace_id, parent_span_id)`.
- **Durable, self-healing pipeline.** Completed spans land in a local SQLite WAL
  outbox. A detached exporter drains it over HTTP/2; a down collector just means
  the outbox grows and the next run retries. Ingest is idempotent
  (`INSERT OR REPLACE` on span id), so retries are safe.
- **The hook never disturbs the session.** `gently hook` writes nothing to stdout
  (Claude parses hook stdout as control output) and always exits 0. It catches
  every panic, logs to `~/.gently/hook.log`, and spawns the exporter fully
  detached so it never blocks on the network.

## Layout

| Crate / dir         | Responsibility |
|---------------------|----------------|
| `gently-core`       | deterministic ids, span type, OTLP/JSON encoding |
| `gently-store`      | `~/.gently/state.db`: open-span tracking + outbox (WAL) |
| `gently-harness`    | `Harness` trait + `ClaudeCode` adapter + stateful applier |
| `gently-export`     | `Transport` trait + HTTP/2 OTLP transport + outbox drain |
| `gently-cli`        | the `gently` binary: hook / export / query / mcp / init |
| `worker/`           | `gently-collector` Cloudflare Worker (TypeScript, D1) |

## Quick start

### 1. Deploy the collector

```bash
cd worker
npm install
wrangler d1 create gently                 # paste database_id into wrangler.toml
wrangler d1 execute gently --remote --file schema.sql
wrangler secret put GENTLY_TOKEN          # a shared bearer token
wrangler deploy
```

Local development instead:

```bash
cd worker
echo 'GENTLY_TOKEN=dev-token' > .dev.vars
wrangler d1 execute gently --local --file schema.sql
wrangler dev                              # http://127.0.0.1:8787
```

### 2. Install the harness integration

```bash
cargo install --path crates/gently-cli   # installs `gently`
gently init --claude                      # hooks + MCP server + ~/.gently/config.toml
```

Edit `~/.gently/config.toml` (or set `GENTLY_COLLECTOR_URL` / `GENTLY_TOKEN`):

```toml
collector_url = "https://gently-collector.<account>.workers.dev"
token = "the-shared-bearer-token"
```

Restart your Claude Code session. Spans now flow on every tool call.

### 3. Query

```bash
gently traces                  # recent sessions
gently trace <trace_id>        # span tree for one session
gently spans --tool-name Bash  # filter spans
gently stats                   # per-tool rollups
```

Add `--json` to any query for machine output. The same surface is available to
the harness as MCP tools: `list_traces`, `get_trace`, `search_spans`,
`trace_stats`.

## Verifying the live hook schema

The Claude Code hook payload schema is confirmed empirically rather than assumed.
Run a session with `GENTLY_DEBUG=1` set and inspect the captured raw payloads:

```bash
GENTLY_DEBUG=1 claude    # then look at ~/.gently/raw/<Event>.jsonl
```

The adapter is tolerant by design - only `session_id` and `hook_event_name` are
required, and any of the 30 hook events it does not explicitly model is recorded
as a marker span.

## Development

```bash
cargo test                                 # all crates
cargo clippy --all-targets -- -D warnings
cd worker && npm test                      # worker (vitest + Miniflare)
```
