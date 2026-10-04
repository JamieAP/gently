# gently

OpenTelemetry traces for Claude Code and Codex.

[Quick start](#quick-start) · [MCP](#mcp) · [Documentation](docs/README.md) · [Contributing](CONTRIBUTING.md)

Gently records coding-agent activity as linked spans, so you can follow a
session through its turns, tool calls, and subagents. Inspect traces from the
command line or let an agent query them through MCP.

The Rust CLI captures hook events into a local SQLite outbox and exports
OTLP/JSON to a collector you run. The included collector is a Cloudflare Worker
backed by D1; it can run on Cloudflare or locally with Wrangler.

```text
Claude Code / Codex hooks -> local SQLite outbox -> Worker -> D1
                                                   ^
                                              CLI and MCP
```

## Example trace

A synthetic session rendered by `scripts/waterfall.py`:

```text
      dur st  span                      │timeline →                                              │
───────── ─  ──────────────────────────┼────────────────────────────────────────────────────────┤
  1000.0ms ✓  session                   │████████████████████████████████████████████████████████│
   900.0ms ✓    turn:1                  │   ██████████████████████████████████████████████████   │
    20.0ms ✓      Read                  │    █                                                   │
   200.0ms ✓      Bash                  │        ███████████                                     │
   450.0ms ✓      agent:1               │                      █████████████████████████         │
   300.0ms ✓        Bash                │                         █████████████████              │
```

## What you can inspect

- Session and turn structure, including parent-child relationships between tools
  and subagents.
- Tool durations, completion status, and trace timelines.
- Recent activity and per-tool statistics, with filters and JSON output.

See [data and privacy](#data-and-privacy) before enabling capture.

## Quick start

You need Rust and Cargo, Node.js and npm, and a Cloudflare account authenticated
with Wrangler. Start from a source checkout:

```sh
git clone https://github.com/JamieAP/gently.git
cd gently
```

### 1. Deploy a collector

```sh
cd worker
npm ci
npx wrangler d1 create gently
```

Copy the returned database ID into `wrangler.toml`, then initialize the database,
set a shared bearer token, and deploy:

```sh
npx wrangler d1 execute gently --remote --file schema.sql
npx wrangler secret put GENTLY_TOKEN
npx wrangler deploy
cd ..
```

For a local collector, initialize D1 with `--local`, configure the Worker's local
`GENTLY_TOKEN`, and run `npx wrangler dev` from `worker/`. Use
`http://127.0.0.1:8787` as the collector URL. See the
[setup guide](docs/getting-started/quickstart.md) for details.

### 2. Install the hooks

```sh
cargo install --path crates/gently-cli
gently init --claude
# For Codex instead: gently init --codex
```

Init installs the hooks and MCP server and creates `~/.gently/config.toml` if it
is missing. Set the collector URL and the same bearer token there:

```toml
collector_url = "https://gently-collector.<account>.workers.dev"
token = "replace-with-your-collector-token"
```

Restart your agent session after configuring it. Codex hooks must also be
trusted inside Codex. `GENTLY_COLLECTOR_URL` and `GENTLY_TOKEN` override the config
values. See [configuration](docs/getting-started/configuration.md) for queue,
transport, timeout, and state-directory options.

### 3. Query a session

Run a task in the configured agent, then inspect its captured activity:

```sh
gently traces
gently trace <trace_id>
gently spans --tool-name Bash
gently stats
gently status
```

Replace `<trace_id>` with an ID from `gently traces`. Trace queries read from the
collector; `gently status` reports local queue and exporter health.

To render a trace timeline from the checkout:

```sh
gently trace <trace_id> --json | python3 scripts/waterfall.py
```

## MCP

The MCP server is built into the CLI as `gently mcp`. Both init commands above
register it with the chosen agent. Restart the agent to load the registration;
for Claude Code, check it with `claude mcp get gently`.

| Tool | Purpose |
| --- | --- |
| `list_traces` | Find sessions and traces |
| `get_trace` | Read a trace's spans |
| `search_spans` | Filter recorded spans |
| `trace_stats` | Summarize tool usage |
| `response_fields` | Inspect available response fields |
| `span_attr_keys` | Discover recorded attribute keys |

The server uses stdio and its tools are read-only. Queries return digest
attributes by default. See [querying and MCP](docs/guides/querying-and-mcp.md) for
arguments, ordering, and local `jq` filters.

## Data and privacy

Normal exports contain digests and byte lengths rather than raw prompt, command,
file, or tool-output values. They also include identifying metadata such as
working directories, host information, tool names, timings, and session IDs.
Digests are fingerprints, not encryption.

Selected raw prompt, tool, and assistant values are stored locally in SQLite,
even when raw resolution is disabled. This content is plaintext and has no
automatic retention limit. To let MCP queries resolve matching local raw values,
explicitly install with `--resolve-local-raw-values`; those results may enter the
calling agent's model-provider context. Reinstalling without that option removes
the registered opt-in.

The collector's shared bearer token grants access to all traces. The collector
provides no tenant separation or automatic retention. Protect both the collector
and local state, and read [security and privacy](docs/concepts/security-and-privacy.md)
for file permissions, exported fields, and debug-capture behaviour.

## Architecture and documentation

One trace represents a session. Turns, tools, and subagents form its span tree.
Hooks write locally; a detached exporter drains the outbox with retry and backoff.
The default queue cap is 10,000 spans, after which the oldest queued spans are
dropped.

Session, turn, and subagent spans are updated as they open and close. The Worker
replaces records with the same span ID. Exporting to another OpenTelemetry
backend requires handling these updates; CLI and MCP queries also depend on the
Worker's query API.

- [Documentation index](docs/README.md)
- [Architecture](docs/concepts/architecture.md)
- [Trace model](docs/concepts/trace-model.md)
- [Configuration](docs/getting-started/configuration.md)
- [Querying and MCP](docs/guides/querying-and-mcp.md)
- [Security and privacy](docs/concepts/security-and-privacy.md)

## Development

The Rust workspace is in `crates/`, the collector in `worker/`, and trace-rendering
helpers in `scripts/`.

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cd worker
npm ci
npm test
```

For hook-schema diagnosis, see the
[debug-capture documentation](docs/concepts/security-and-privacy.md). Debug
capture writes additional raw data and should be enabled deliberately.

## License

[MIT](LICENSE). Dependencies retain their own licenses and notices.
