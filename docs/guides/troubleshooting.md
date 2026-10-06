# Troubleshooting

Start with local health, then test a collector query from an authenticated
process. These checks distinguish missing capture, queued export and query
authentication problems.

```sh
gently --version
gently status
```

`status` uses the local state directory. It does not contact the collector or
need a token. Do not print tokens or process environments when diagnosing
authentication.

## Read the health fields

| Field | What to look for |
| --- | --- |
| `capture_degraded`, `last_capture`, `last_capture_outcome` | Check whether hooks stored encrypted content, metadata only, or encountered a fixed capture-failure category. A recent hook timestamp alone does not prove delivery. |
| `capture_<outcome>` | Cumulative counts per fixed category. `policy_unavailable`, `policy_expired`, `oversized`, `raw_capacity`, `seal_failed` and `raw_store_failed` kept metadata but dropped ciphertext; `invalid_hook` and `capture_failed` lost the event. See [`gently status`](../reference/cli.md#gently-status---json). |
| `recipient_policy`, `policy_expires_unix_secs` | Signed policy is checked without unlocking a reader. Renewal is needed when expired; `expiring_soon` means at most seven days remain. |
| `pending (outbox)` | A growing count means events are queued faster than they are delivered, or export is unavailable. |
| `last_success` | A recent value proves some export succeeded; `never` means this state database has no successful export recorded. |
| `last_export` | Time since an export attempt, including failures. |
| `last_error` | The latest recorded exporter failure. |
| `consecutive_failures` | Repeated failures require checking the error and collector configuration. |
| `quarantined` | Envelopes rejected or malformed during export; they are no longer in the active queue. |
| `raw_bytes`, `raw_budget_bytes` | Encoded ciphertext storage use and its 64 MiB admission budget; SQLite overhead is additional. Full storage skips new raw capture or caching. |
| `raw_pending`, `raw_pending_bytes` | Ciphertext objects and bytes not yet acknowledged by the collector. |

Counts refer to envelopes, not spans. A zero pending count alone does not prove
capture is working: generate a new tool-using task and confirm that a trace
appears in the collector.

## Common symptoms

| Symptom | Check and action |
| --- | --- |
| `gently` or `--waterfall` is not found | Install or update from the repository with `cargo install --path crates/gently-cli --locked`; check `PATH` and `gently --version`. |
| No new local activity | Run `gently init` for the intended agent, restart it, and make sure it uses the installed hooks. Review/trust Codex hooks inside Codex. |
| Queue grows, with no export attempts | Tokenless hooks only queue. Start a token-bearing export watcher or use the local collector launcher. |
| `collector_url is not configured` | Set the base URL in the active state directory's config, or supply `GENTLY_COLLECTOR_URL`. |
| `token is not configured` | Export needs a token supplied through your secret manager. Unix CLI/MCP queries can use an unlocked watcher with `--serve-queries`, provided the state directory and collector URL match. |
| Export or query returns `401`/`403` | Confirm the process and collector use the same token without displaying it. Export stops on rejection and retains the queue; correct the token and restart the watcher. |
| Local connection refused | Start `./scripts/collector-local` and leave its terminal open. Check that the configured URL is `http://127.0.0.1:8787`. |
| Raw retention is missing | Check `recipient_policy` and `last_capture_outcome` in `gently status`. `unavailable` or `policy_unavailable` means verify capture opt-in, signed manifest, owner pin, minimum epoch and exact policy digest; `expired` or `policy_expired` means enroll a refreshed policy. `raw_capacity` means the raw byte budget is full, `oversized` means the content is over the size limit and `raw_store_failed` points at local storage. Metadata continues in each case. |
| Raw resolution fails | Check the enrolled reader identity, tenant read credential and object availability. Software unlocking requires an attached private terminal; reject wrong-key or binding errors. |
| Span ownership conflict (`409`) | A different device or trace owns that span ID. Use fresh capture IDs for a new device/trace; do not spoof its owner. |
| Incompatible development state | Stop Gently and explicitly reset disposable old application state and sidecars; preserve the credential vault. |
| Cloudflare database errors | Replace `REPLACE_AFTER_CREATE` with the created D1 ID and execute `schema.sql` against the intended local or remote database. |
| Quarantine count increases | Run `gently quarantine list` for each row's category and collector HTTP status, then check payload/schema compatibility or collector limits. Fixing the cause does not replay envelopes automatically; requeue each with `gently quarantine retry --id ID`. |
| Export works but queries are empty | Check the target URL, query filters and state directory. Queries read the collector, not the local outbox. Generate fresh activity and list unfiltered traces. |
| MCP is absent or fails | Re-run init and restart the client. Check registration and provide a token or start the watcher with `--serve-queries` on Unix. |
| Codex capture is duplicated or labeled `claude-code` | Check both `~/.codex/hooks.json` and inline TOML hooks. Run `gently init --codex` to remove covered legacy Gently handlers, then restart Codex. Custom registrations should invoke `gently hook --harness codex`. |
| The local launcher stops both services | Its exit diagnostic identifies the exporter or collector and exit status or signal. Check that cause before restarting; queued hooks remain available for a later drain. |

For retry and queue behavior, see [Reliability](../concepts/reliability.md).
For settings and file locations, see [Configuration](../getting-started/configuration.md).

## Desktop capture and MCP

Desktop hooks can record without inheriting a token. A foreground-unlocked
watcher can export those records and, with `--serve-queries` on Unix, serve
read-only queries for tokenless desktop MCP clients. Restart older watchers
and ensure both processes use the same state directory and collector URL.
A stale socket is recovered when the watcher restarts; a non-socket path is
refused. Verify actual queries as well as export health.

For Claude Code, inspect the registration with `claude mcp get gently` after
restarting the client. For Codex, check the installed hook and MCP entries in its
configuration and trust the hooks inside Codex. Neither check should require
pasting a credential into a config or conversation.

## Waterfall reports failed integrity checks

The checks describe the spans returned by the collector. A partial capture,
missing parent or unclosed session can affect the results. They do not prove
that every event was captured, and an unset status (`·`) is not a failure.

Try an unfiltered `gently trace TRACE_ID --json` and inspect the recorded span
IDs, parent IDs and bounds. The renderer prefers effective bounds when present
and allows 2 ms of clock slack for nesting. Duplicate span IDs, parent cycles,
malformed timestamps and empty input return an error rather than a chart.
See [Trace model](../concepts/trace-model.md) and
[Querying and MCP](querying-and-mcp.md).

## Diagnose an adapter with raw payloads

Use a synthetic reproduction first. Selected raw fields can contain prompts,
source material and credentials. If real fields are necessary, enroll readers
and explicitly enable encrypted raw capture with valid signed policy. The old
plaintext full-payload debug capture has been removed. Gently does not generate
environment snapshots.

Disable capture after diagnosis and review stored values and files before
sharing them. Turning capture off does not remove existing data. See
[Security and privacy](../concepts/security-and-privacy.md) and
[Harness hooks](../reference/hooks.md).

For a bug report, include versions, the agent, expected behavior, observed
behavior and a minimal invented payload. Do not attach your state database,
real tokens or unreviewed raw output. See
[Contributing](https://github.com/JamieAP/gently/blob/main/CONTRIBUTING.md).
