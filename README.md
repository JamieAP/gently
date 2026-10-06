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
────────── ─  ──────────────────────────┼────────────────────────────────────────────────────────┤
  1000.0ms ✓  session                   │████████████████████████████████████████████████████████│
   900.0ms ✓    turn:1                  │   ██████████████████████████████████████████████████   │
    20.0ms ✓      Read                  │    █                                                   │
   200.0ms ✗      Bash                  │      ███████████                                       │
   450.0ms ✓      agent:1               │                      █████████████████████████         │
   300.0ms ·        Bash                │                         █████████████████              │

Status: ✓ ok · unset ✗ error ? unknown
```

## Current support

Capture and queries cover Claude Code and Codex coding agents in the terminal
and desktop. On Unix, desktop MCP can delegate queries to an unlocked export
watcher through a private socket. See [compatibility](docs/reference/compatibility.md)
for checked versions, hook coverage and fidelity limits.

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
enroll per-host bearer credentials, and deploy:

```sh
npx wrangler d1 execute gently --remote --file schema.sql
npx wrangler secret put GENTLY_HOSTS
npx wrangler deploy
cd ..
```

Configure `GENTLY_HOSTS` through a private secret-provider workflow as a JSON
array of `{token, tenant_id, device_id, capabilities}` records. Use distinct
host credentials and `ingest`/`read` capabilities; keep credential values out of
source and shell arguments. See [collector setup](docs/getting-started/quickstart.md).

For a local collector, follow [local setup](LOCAL_SETUP.md). The launchers
accept an inherited token from your provider on Macs or Linux and start a
loopback Worker, local D1 and an export watcher. A separate Mac hardware-backed
secret helper is an optional wrapper.

### 2. Install the hooks

```sh
cargo install --path crates/gently-cli --locked
gently init --claude
# For Codex instead: gently init --codex
```

Init installs the hooks and MCP server and creates `~/.gently/config.toml` if it
is missing. Set the collector URL and enrolled tenant/device there. Supply the
process's bearer credential only as
`GENTLY_TOKEN` to export and query processes:

```toml
collector_url = "https://gently-collector.<account>.workers.dev"
tenant_id = "personal"
device_id = "mac-main"
# prefer_quic = true
```

Restart your agent session after configuring it. Codex hooks must also be
trusted inside Codex. `GENTLY_COLLECTOR_URL`, `GENTLY_TENANT_ID` and `GENTLY_DEVICE_ID` override
public config values; `GENTLY_TOKEN` has no persisted-token fallback. See [configuration](docs/getting-started/configuration.md) for queue,
transport, timeout, and state-directory options. A watcher can export tokenless
desktop hooks. With `--serve-queries`, it also handles tokenless CLI/MCP queries
over an owner-only Unix socket. Background
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
collector; `gently status` reports local capture, recipient-policy and export health.

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

The server uses stdio and its tools are read-only. Queries return metadata, byte lengths and optional opaque raw references by
default. See [querying and MCP](docs/guides/querying-and-mcp.md) for
arguments, ordering, and local `jq` filters.

## Data and privacy

Normal exports contain metadata and byte lengths, with no public content
fingerprints. Optional raw fields are encrypted before local persistence to
only the readers in an owner-signed, locally pinned tenant policy. Capture,
ciphertext cloud sync and reader resolution are independent and disabled by
default. Cloudflare receives opaque ciphertext and never a raw-decryption key.

[Enroll Mac/Linux readers](docs/guides/encrypted-raw-values.md), including a
recovery device, before enabling capture. Readers unlock their identity only
for explicit resolution; decrypted CLI/MCP output can enter the calling
agent's model-provider context. Native Mac hardware keys and passphrase-encrypted
software keys have different protection guarantees.

The collector maps host credentials to tenant/device and ingest/read rights.
Tenant boundaries are enforced in all storage and queries. Paths, host data,
byte lengths and activity patterns remain visible, and raw objects have no
writer signatures or full replay/provenance protocol against a malicious cloud.
There is no automatic retention. Read
[security and privacy](docs/concepts/security-and-privacy.md) for those limits.

Gently is pre-public: existing development state must be explicitly reset to
use the fresh encrypted schema. No plaintext migration, digest alias or
compatibility reader is retained; the separate credential vault is preserved.

## Architecture and documentation

One trace represents a session. Turns, tools, and subagents form its span tree.
Hooks queue one OTLP/JSON envelope per event. With a collector token, they start
a detached exporter; tokenless hooks queue locally. `gently export --watch`
drains the queue continuously and reuses connections. Remote export can use
HTTP/3, with a TCP fallback.

The exporter drains the outbox with retry and backoff.
Explicit exports default to a 10,000-envelope cap; excess oldest envelopes
are dropped before sending. Hook-spawned exporters and the local launchers use
`--preserve-backlog`, overriding that cap to avoid discarding queued history. The queue can grow
beyond that cap while no exporter drains it. Authentication failures stop export
immediately; transient failures use backoff, and unprocessable envelopes can be
quarantined.

Session, turn, and subagent spans are updated as they open and close. The Worker
merges repeated span reports within a tenant and preserves each span's capture
device ownership. Exporting to another OpenTelemetry
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

Install the public-repository Git gates in every development checkout:

```sh
git config --local core.hooksPath .githooks
```

After reviewing the exact change for public disclosure, acknowledge each command
with `GENTLY_PUBLIC_REPO_SANITY=1 git commit ...` or
`GENTLY_PUBLIC_REPO_SANITY=1 git push ...`. The acknowledgement does not bypass
checks. The gates inspect staged files, commit messages and complete outgoing
history, including deleted files and annotated tags. Captured hook/OTLP JSON,
encrypted raw objects, credentials, private state, databases, archives and local
home paths are rejected; diagnostics withhold matched values. Keep all runtime
state outside tracked files and review other confidential prose or code manually.

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cd worker
npm ci
npm test
npm run typecheck
```

Read the [capture and privacy documentation](docs/concepts/security-and-privacy.md)
before enabling selected raw fields. The old plaintext full-payload debug
capture has been removed.

## License

[MIT](LICENSE). Dependencies retain their own licenses and notices.
