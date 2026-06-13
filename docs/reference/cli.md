# CLI reference

A single binary, `gently`, with these subcommands.

## `gently hook`

Harness hook entrypoint. Reads the event JSON on stdin, updates the local outbox,
and spawns the detached exporter. **Contract:** writes nothing to stdout, always
exits 0. You don't run this by hand - `gently init` wires it into the harness.

## `gently export`

Drains the local outbox to the collector. A `flock` singleton: if another
exporter holds the lock it exits immediately. Retries retryable failures
(unreachable, timeout, 5xx, and recoverable auth/throttle `401`/`403`/`408`/`429`)
with exponential backoff; quarantines only genuinely unprocessable 4xx
(`400`/`413`/`422`); records health. Normally spawned by the hook, but safe to run
manually to force a flush.

## `gently status`

Prints local exporter health and queue depth - collector URL, `prefer_quic`,
pending outbox count, quarantined count, consecutive failures, last export / last
success (relative), last error.

## `gently traces`

Lists recent traces (one per session), newest first.

* `--limit <N>` · `--harness <name>` · `--json`
* Also accepts `--session-id <id>`, `--since <unix_nano>`,
  `--until <unix_nano>`, and
  `--order <start_desc|start_asc|last_activity>`.

## `gently trace <trace_id>`

Shows one trace. Default renders an indented span tree; `--json` emits the raw
span array (feed to `scripts/waterfall.py`).

## `gently spans`

Filtered span search.

* `--trace-id <id>` · `--tool-name <name>` · `--status <0|1|2>` ·
  `--since <unix_nano>` · `--limit <N>` · `--json`
* Also accepts `--session-id <id>`, `--harness <name>`, `--name <span_name>`,
  `--kind <code>`, `--until <unix_nano>`, and
  `--order <start_desc|start_asc>`.

## `gently stats`

Per-tool rollups: span count, error count, average duration. `--json` available.

## `gently mcp`

Runs the stdio MCP server (see [Querying & MCP](../guides/querying-and-mcp.md)).
Invoked by the harness, not by hand. MCP query tools accept an optional local
`jq` filter and expose `response_fields` / `span_attr_keys` discovery tools.

## `gently init --claude`

Idempotently installs the integration: hook entries in
`~/.claude/settings.json`, the MCP server in `~/.claude.json`, and a scaffolded
`~/.gently/config.toml`. Re-running adds nothing duplicate.

---

Configuration for all commands comes from `~/.gently/config.toml` + environment -
see [Configuration](../getting-started/configuration.md).
