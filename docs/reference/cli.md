# CLI reference

A single binary, `gently`, with these subcommands.

## `gently hook`

Harness hook entrypoint. Reads the event JSON on stdin, updates the local outbox,
and starts a detached exporter only when a collector token is available.
Without a token it records locally without spawning an exporter. **Contract:**
writes nothing to stdout, always
exits 0. You don't run this by hand - `gently init` wires it into the harness.

## `gently export`

Drains the local outbox to the collector. A `flock` singleton: if another
exporter holds the lock it exits immediately. Authentication failures
(`401`/`403`) stop immediately and retain the queue; they do not trigger transport
fallback or retries with the same token. Unreachable collectors, timeouts, 5xx
and non-payload endpoint errors such as `404`/`405`/`409`/`408`/`429` use
exponential backoff. Unprocessable envelopes (`400`/`413`/`422`) and malformed
queued JSON are quarantined; all outcomes update health. An event envelope is
quarantined as a whole, including valid sibling spans. Run manually to force a
flush.

`gently export --watch --interval-secs 2` keeps polling the outbox, including
spans queued by tokenless desktop hooks. Unlock a hardware-backed token once
in a foreground terminal and pass it to this process; the watcher does not
unlock secrets itself. Stop with Ctrl+C. Authentication failure stops the
watcher and leaves queued spans intact; restart it with a corrected token.

## `gently status`

Prints local exporter health and queue depth - collector URL, `prefer_quic`,
pending outbox envelope count, quarantined count, consecutive failures, last export / last
success (relative), last error.

## `gently traces`

Lists recent traces (one per session), newest first.

* `--limit <N>` · `--harness <name>` · `--json`
* Also accepts `--session-id <id>`, `--since <unix_nano>`,
  `--until <unix_nano>`, and
  `--order <start_desc|start_asc|last_activity>`.

## `gently trace <trace_id>`

Shows one trace. The default renders an indented span tree. `--json` emits the
raw span array; `--waterfall` renders time-proportional bars and a trace integrity
summary. The two flags are mutually exclusive.

Waterfall labels use terminal display widths and mark clipped names with an
ellipsis. A compact legend explains the status symbols.

## `gently waterfall`

Reads a JSON span array from stdin, using the `gently trace --json` shape, and
renders the same waterfall and integrity summary. No collector, token, or
configuration is needed:

```sh
gently waterfall < trace.json
gently trace <trace_id> --json | gently waterfall
```

The summary checks session-root presence, parent links, negative durations, and
temporal nesting with 2 ms of clock slack. These are diagnostics rather than a
guarantee that every event was captured. Failed checks are reported in the
summary; empty input, malformed timestamps, duplicate span IDs, and parent
cycles return an error.

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

## `gently init --claude` / `gently init --codex`

Idempotently installs the integration: hook entries in
`~/.claude/settings.json`, the MCP server in `~/.claude.json`, and a scaffolded
`~/.gently/config.toml`. Codex uses `~/.codex/config.toml` and requires trusting
installed hooks inside Codex. Re-running adds no duplicate hooks.

MCP raw-value resolution is disabled unless `--resolve-local-raw-values` is
provided. Re-running init without it removes the installed opt-in. This option
can expose previously captured local prompt/tool/assistant content to the agent
and its provider. It does not enable raw capture; that requires
`capture_raw_values = true` or `GENTLY_CAPTURE_RAW_VALUES=1` on the hook process.

---

Commands that access the collector or local state use `~/.gently/config.toml`
and environment settings. The stdin-only `waterfall` command does not load
configuration. See [Configuration](../getting-started/configuration.md).
