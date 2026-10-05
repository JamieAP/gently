<p align="center">
  <img src="assets/gently-mark.png" alt="Gently logo" width="80" height="80">
</p>

<h1 align="center">gently</h1>

<p align="center">
  <strong>See your agent's work, one trace at a time.</strong><br>
  OpenTelemetry traces for Claude Code and Codex.
</p>

<p align="center">
  <a href="#quick-start">Get started</a> ·
  <a href="LOCAL_SETUP.md">Run locally</a> ·
  <a href="#mcp">MCP</a> ·
  <a href="docs/README.md">Docs</a> ·
  <a href="CONTRIBUTING.md">Contribute</a>
</p>

Gently turns coding-agent hooks into a timeline of sessions, turns, tool calls
and subagents. Investigate a slow run, follow delegated work, or query recorded
activity from the command line and through MCP.

- **Capture locally.** Hooks write to a SQLite outbox, so events can queue before
  a collector or its token is available.
- **Follow the work.** Inspect tool durations, completion status, and the span
  tree behind a session.
- **Query from your agent.** Read the same traces through MCP, with filters and
  JSON output. Raw content capture and resolution are opt-in.

Run the included collector locally or on Cloudflare. It is a Worker backed by
D1; the Rust CLI exports OTLP/JSON to it.

## A session at a glance

A synthetic session rendered by `gently trace --waterfall`:

```text
      dur st  span                      │timeline →                                              │
───────── ─  ──────────────────────────┼────────────────────────────────────────────────────────┤
  1000.0ms ✓  session                   │████████████████████████████████████████████████████████│
   900.0ms ✓    turn:1                  │   ██████████████████████████████████████████████████   │
    20.0ms ✓      Read                  │    █                                                   │
   200.0ms ✗      Bash                  │      ███████████                                       │
   450.0ms ✓      agent:1               │                      █████████████████████████         │
   300.0ms ·        Bash                │                         █████████████████              │

Status: ✓ ok · unset ✗ error ? unknown
```

## Current support

CLI capture and token-configured queries are available for Claude Code and
Codex. Desktop parity is partial: authenticated desktop MCP access and
Claude Chat/Cowork integration remain open work.

Read [data and privacy](#data-and-privacy) before enabling capture.

## Quick start

You need Rust and Cargo, Node.js and npm. Deploying to Cloudflare also requires
an account authenticated with Wrangler. Start from a source checkout:

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

For a local collector with a Secure Enclave token, follow
[Local setup](LOCAL_SETUP.md). It starts a localhost Worker, local D1, and an
export watcher from a foreground terminal so hardware unlock can prompt once.
Use `http://127.0.0.1:8787` as the collector URL.

### 2. Install the hooks

```sh
cargo install --path crates/gently-cli --locked
gently init --claude
# For Codex instead: gently init --codex
```

Init installs the hooks and MCP server and creates `~/.gently/config.toml` if it
is missing. Set the collector URL there and supply the shared bearer token as
`GENTLY_TOKEN` to export and query processes:

```toml
collector_url = "https://gently-collector.<account>.workers.dev"
# prefer_quic = true
```

Restart your agent session after configuring it. Codex hooks must also be
trusted inside Codex. `GENTLY_COLLECTOR_URL` and `GENTLY_TOKEN` override the config
values. See [configuration](docs/getting-started/configuration.md) for queue,
transport, timeout, and state-directory options. A watcher can export tokenless
desktop hooks; CLI and MCP query processes still need the token. Background
hooks never attempt hardware unlock.

### 3. Query a session

Run a task in the configured agent, then inspect its captured activity:

```sh
gently traces
gently trace TRACE_ID
gently spans --tool-name Bash
gently stats
gently status
```

Replace `TRACE_ID` with an ID from `gently traces`. Trace queries read from the
collector; `gently status` reports local queue and exporter health.

To render a trace timeline with integrity checks:

```sh
gently trace TRACE_ID --waterfall
```

For saved or piped span JSON, use `gently waterfall < trace.json`. Neither
waterfall command requires Python.

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

Raw prompt, tool, and assistant capture is disabled by default. Enable local
SQLite capture with `capture_raw_values = true` or `GENTLY_CAPTURE_RAW_VALUES=1`.
Captured content is plaintext and has no automatic retention limit; disabling
capture does not delete existing values. Raw resolution is a separate opt-in:
install with `--resolve-local-raw-values` to let MCP queries resolve captured
values. Those results may enter the calling agent's model-provider context.
Reinstalling without that option removes the registered resolution opt-in.

The collector's shared bearer token grants access to all traces. The collector
provides no tenant separation or automatic retention. Protect both the collector
and local state, and read [security and privacy](docs/concepts/security-and-privacy.md)
for file permissions, exported fields, and debug-capture behaviour.

## Architecture and documentation

One trace represents a session. Turns, tools, and subagents form its span tree.
Hooks queue one OTLP/JSON envelope per event. With a collector token, they start
a detached exporter; tokenless hooks queue locally. `gently export --watch`
drains the queue continuously and reuses connections. Remote export can use
HTTP/3, with a TCP fallback.

The exporter drains the outbox with retry and backoff.
The default queue cap is 10,000 envelopes, applied when export drains the
outbox; excess oldest envelopes are dropped before sending. The queue can grow
beyond that cap while no exporter drains it. Authentication failures stop export
immediately; transient failures use backoff, and unprocessable envelopes can be
quarantined.

Session, turn, and subagent spans are updated as they open and close. The Worker
replaces records with the same span ID. Exporting to another OpenTelemetry
backend requires handling these updates; CLI and MCP queries also depend on the
Worker's query API.

```text
Claude Code / Codex hooks -> local SQLite outbox -> Worker -> D1
                                                   ^
                                              CLI and MCP
```

- [Documentation index](docs/README.md)
- [Architecture](docs/concepts/architecture.md)
- [Trace model](docs/concepts/trace-model.md)
- [Current harness hooks](docs/reference/hooks.md)
- [Configuration](docs/getting-started/configuration.md)
- [Querying and MCP](docs/guides/querying-and-mcp.md)
- [Troubleshooting](docs/guides/troubleshooting.md)
- [Security and privacy](docs/concepts/security-and-privacy.md)

## Development

The Rust workspace is in `crates/`, the collector in `worker/`, and local collector
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
