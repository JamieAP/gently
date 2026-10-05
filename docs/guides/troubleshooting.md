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
| `pending (outbox)` | A growing count means events are queued faster than they are delivered, or export is unavailable. |
| `last_success` | A recent value proves some export succeeded; `never` means this state database has no successful export recorded. |
| `last_export` | Time since an export attempt, including failures. |
| `last_error` | The latest recorded exporter failure. |
| `consecutive_failures` | Repeated failures require checking the error and collector configuration. |
| `quarantined` | Envelopes rejected or malformed during export; they are no longer in the active queue. |

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
| `token is not configured` | Supply `GENTLY_TOKEN` to this command or MCP server through your secret manager. A separate watcher's token does not apply here. |
| Export or query returns `401`/`403` | Confirm the process and collector use the same token without displaying it. Export stops on rejection and retains the queue; correct the token and restart the watcher. |
| Local connection refused | Start `./scripts/collector-local` and leave its terminal open. Check that the configured URL is `http://127.0.0.1:8787`. |
| Cloudflare database errors | Replace `REPLACE_AFTER_CREATE` with the created D1 ID and execute `schema.sql` against the intended local or remote database. |
| Quarantine count increases | Check the recorded error and payload/schema compatibility. Fixing the cause does not automatically replay quarantined envelopes. |
| Export works but queries are empty | Check the target URL, query filters and state directory. Queries read the collector, not the local outbox. Generate fresh activity and list unfiltered traces. |
| MCP is absent or fails | Re-run init and restart the client. Check registration and provide the token to the MCP process, not just the exporter. |

For retry and queue behavior, see [Reliability](../concepts/reliability.md).
For settings and file locations, see [Configuration](../getting-started/configuration.md).

## Desktop capture and MCP

Desktop hooks can record without inheriting a token. A foreground-unlocked
watcher can export those records, but it cannot authenticate a separately
launched desktop MCP server. Authenticated desktop MCP access and Claude
Chat/Cowork integration remain incomplete. Verify capture and export separately
instead of treating a running collector as proof that desktop queries work.

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

Use a synthetic reproduction first. Full debug payloads can contain prompts,
source material, credentials and local paths. If real payloads are necessary,
explicitly enable both raw capture and debug capture for the hook process.
Debug alone does not write payloads. Gently does not generate environment
snapshots.

Disable capture after diagnosis and review stored values and files before
sharing them. Turning capture off does not remove existing data. See
[Security and privacy](../concepts/security-and-privacy.md) and
[Harness hooks](../reference/hooks.md).

For a bug report, include versions, the agent, expected behavior, observed
behavior and a minimal invented payload. Do not attach your state database,
real tokens or unreviewed debug captures. See
[Contributing](https://github.com/JamieAP/gently/blob/main/CONTRIBUTING.md).
