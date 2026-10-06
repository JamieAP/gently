# CLI reference

`gently` captures hook events, exports queued spans, and queries the collector.
Use `gently --help` or `gently <command> --help` for command syntax;
`gently --version` prints the installed version.

## Connection requirements

| Command | Collector URL and token required? |
| --- | --- |
| `hook` | No. Events queue locally without credentials. |
| `init`, `status`, `raw` | No. These use local configuration and files. |
| `waterfall` | No. Reads stdin without loading configuration or local state. |
| `export` | Yes. |
| `traces`, `trace`, `spans`, `stats`, `whoami`, `mcp` | URL plus token, or a local query watcher on Unix. |

Set `GENTLY_COLLECTOR_URL` and supply `GENTLY_TOKEN` to the process that exports
or queries. Installing hooks does not supply a token to agent processes.
See [configuration](../getting-started/configuration.md) and
[local setup](../getting-started/local-collector.md) for the collector and credential setup.

## `gently init --claude` / `gently init --codex`

Install the hooks and stdio MCP registration for one harness:

```sh
gently init --claude
# Or install into Codex:
gently init --codex
```

With neither flag, `init` selects Claude Code. If `--codex` is present, it selects
Codex. Run the commands separately to install both integrations.

| Integration | Files updated |
| --- | --- |
| Claude Code | `~/.claude/settings.json` hooks and `~/.claude.json` MCP server |
| Codex | `~/.codex/config.toml` hooks, MCP server, and hook feature setting; covered legacy Gently handlers in `~/.codex/hooks.json` |

Init also creates `<state_dir>/config.toml` if missing, with a localhost
collector URL. It preserves an existing config and avoids duplicate hook
commands. Restart the agent after installation. Codex hooks must be trusted
inside Codex through `/hooks` before they fire.

`--resolve-raw-values` opts the installed MCP server into returning
decrypted, referenced prompt, tool or assistant values. Running init again
without that flag removes the installed resolution setting. This flag does not
enable capture. Read [raw values](../guides/querying-and-mcp.md#raw-values)
before enabling it.

## `gently raw`

Manage encrypted reader identities and signed public enrollment:

```text
gently raw identity --out PATH
gently raw owner-key --recipient AGE_PUBLIC_RECIPIENT [--recipient AGE_PUBLIC_RECOVERY_RECIPIENT] --out PATH
gently raw sign --manifest UNSIGNED_JSON --owner-key OWNER_AGE --identity READER_IDENTITY --out SIGNED_JSON
gently raw trust --manifest SIGNED_JSON --owner-public OWNER_PUBLIC_KEY_BASE64 --out TRUST_JSON
```

`identity` obtains a passphrase through a private terminal prompt, writes an
encrypted software identity and prints its public recipient. `owner-key`
encrypts a new signing key to approved public recipients and prints the public
verification key. `sign` explicitly unlocks a reader to access the encrypted
owner key. `trust` verifies policy against an independently checked public
owner root and refuses root replacement, minimum-epoch rollback or a different
manifest at the pinned epoch. Software reading and signing require an attached
private terminal for passphrase entry. Commands
never print private keys or passphrases. Software keys are not hardware bound.

See [device enrollment](../guides/encrypted-raw-values.md) for setup, native Mac
readers, recovery and the separate capture/sync/resolution opt-ins.

## `gently hook`

```text
gently hook [--harness claude|codex]
```

The installed hook reads one harness event as JSON from stdin. `--harness`
defaults to `claude`; Codex installation adds `--harness codex`.
Normally the harness invokes this command, rather than a person.

It records span updates in the local SQLite outbox. With a configured collector
URL and token, it can start a detached exporter. Without them, it queues locally.
Once dispatched, the hook writes nothing to stdout and suppresses processing
errors so it exits successfully; argument-parsing errors are separate.

## `gently export`

```sh
gently export
gently export --watch --interval-secs 2
# Enable tokenless desktop MCP and CLI queries on Unix:
gently export --watch --serve-queries --preserve-backlog
```

Without `--watch`, drain the queued envelopes and exit. With
`sync_raw_values` enabled, also upload retained ciphertext; no reader key is
needed. Watch mode continues
polling for new events. `--interval-secs` defaults to `2` and accepts `1` through
`60`; it controls watch polling, not request timeout. Stop the watcher with Ctrl+C.

A lock permits one exporter per tenant/device runtime directory. A one-shot export exits
successfully if the lock is already held; a second watcher returns an error.
The process needs an available collector token and never unlocks a secret store
itself. A foreground launcher can unlock once and pass the token to the watcher,
allowing hooks without tokens to keep recording locally.

With `--serve-queries`, tokenless query clients automatically use the private
`<state_dir>/tenants/<tenant_id>/devices/<device_id>/query.sock`. They must share
the state directory, tenant, device and collector URL.
The watcher forwards only read-only metadata queries and ciphertext downloads,
and keeps its credential in memory. Reader decryption remains explicit.
The flag requires `--watch` and Unix. Socket requests are limited to 64 KiB,
collector metadata responses to 8 MiB and ciphertext responses to 1 MiB, with at most 16 concurrent requests and the
configured query timeout. Ctrl+C removes the socket; a restart recovers stale
sockets after abrupt termination. See [security and privacy](../concepts/security-and-privacy.md).

Authentication failures (`401` or `403`) stop export and retain queued rows.
Retryable failures use backoff: one-shot export attempts up to three drains;
a watcher continues retrying, with delay capped at 30 seconds. Malformed queued
JSON and rejected envelopes can be quarantined. An envelope is quarantined as a
whole, including any valid sibling spans it contains.

The default outbox cap is 10,000 envelopes and is enforced when a drain starts,
by dropping the oldest excess rows. `--preserve-backlog` overrides that cap for
this process, keeping all queued history, including before an authentication
failure. Hook-spawned exporters and bundled local launchers set this option;
queue storage can grow without a limit. A tokenless queue can grow past this cap
before export begins. See [reliability](../concepts/reliability.md) for delivery,
retry, and quarantine behavior.

## `gently status`

Print local queue depth, quarantine count, configured collector URL, transport
preference, consecutive failures, last attempt, last success, and last error.
It does not contact the collector or verify that a token works.

## `gently traces`

List trace summaries in a table, or use `--json` for JSON:

```sh
gently traces --limit 10 --harness codex --order last_activity
gently traces --session-id SESSION_ID --json
```

Accepted flags: `--limit`, `--harness`, `--session-id`, `--since`, `--until`,
`--order`, and `--json`. Harness values recorded by the adapters are
`claude-code` and `codex`.

The Worker defaults to 50 rows and caps `--limit` at 1,000. Default order is
`start_desc`. Other orders are `start_asc`, `last_activity`,
`last_activity_desc`, and `last_activity_asc`; `last_activity` means descending.
`--since` and `--until` are decimal Unix nanosecond strings. See
[query semantics](../guides/querying-and-mcp.md#filters-limits-and-ordering)
for how time filters affect summaries.

## `gently trace <trace_id>`

```sh
gently trace TRACE_ID
gently trace TRACE_ID --json
gently trace TRACE_ID --waterfall
```

The default is an indented span tree. `--json` prints the span-row array;
`--waterfall` prints a timing chart and integrity report. These flags are
mutually exclusive. Copy a trace ID from `gently traces`; the single-trace query
has no row-limit flag.

## `gently waterfall`

Render saved or piped span rows without a collector, token, or configuration:

```sh
gently waterfall < trace.json
gently trace TRACE_ID --json | gently waterfall
```

Input must be a JSON span array in the `gently trace --json` format, rather than
an OTLP export envelope. The pipeline's first command needs a token or a local query watcher.

The chart uses 56 timing columns, indented 26-column Unicode labels, ellipses
for clipped names, and a status legend. It prefers effective bounds when present,
including zero, and otherwise uses raw bounds. Root and sibling order follows
raw start times.

Integrity checks report session-root presence, resolved parent links, negative
raw durations, and temporal nesting with 2 ms of slack. An unclosed parent has
no enforced upper bound. Failed checks remain diagnostics in successful output;
they do not prove whether all events were captured. Empty or malformed input,
invalid timestamps, duplicate span IDs, and parent cycles return an error.

## `gently spans`

```sh
gently spans --trace-id TRACE_ID --tool-name Bash --order start_asc
gently spans --status 2 --kind 3 --limit 100 --json
```

Accepted flags: `--trace-id`, `--session-id`, `--harness`, `--tool-name`, `--name`,
`--status`, `--kind`, `--since`, `--until`, `--limit`, `--order`, and `--json`.
Text filters match exact values. Status codes are `0` unset, `1` OK, and `2`
error. Gently uses kind `1` for internal spans and `3` for tool/client spans.

The Worker defaults to 50 rows, caps the limit at 1,000, and sorts by
`start_desc`; `start_asc` is also supported. Results are a limited search, not a
complete trace. Use `gently trace TRACE_ID` to retrieve that trace's full rows.

## `gently stats`

```sh
gently stats
gently stats --json
```

Return span counts, error counts, and mean raw duration per tool, ordered by span
count descending. This command has no filters or percentile metrics.

## `gently whoami --pane <pane>`

```sh
gently whoami --pane '%42'
gently whoami --pane '%42' --json
```

Find the newest captured span whose `gently.tmux_pane` resource attribute
matches the pane. This is a collector query, not a local process or tmux lookup.
It searches the most recent 1,000 spans in the configured tenant, so a stale match
can remain and an older match can fall outside the search window.

Default output is just the matching session ID, or nothing when no session ID
is found. JSON includes `tmux_pane`, `session_id`, `trace_id`, `cwd`, and
`transcript_path` for a match. Without a match it returns the pane and a null
`session_id`. Optional context fields can be null.

This command is unrelated to the Worker's `/v1/whoami` transport probe.

## `gently mcp`

Run the newline-delimited JSON-RPC stdio server. Init registers this command
with the harness. The process requires a collector URL and either a token
or, on Unix, the local query watcher at startup, even for `response_fields`.

Tools, examples, local `jq` behavior, and raw-value resolution are documented in
[querying and MCP](../guides/querying-and-mcp.md).

## Source

[Command parsing](https://github.com/JamieAP/gently/blob/main/crates/gently-cli/src/main.rs) and
[query rendering](https://github.com/JamieAP/gently/blob/main/crates/gently-cli/src/cmd_query.rs) define this interface.
