# Configuration

Gently loads `config.toml` from its state directory, then applies environment
overrides. The default directory is `~/.gently`; `GENTLY_STATE_DIR` changes both
the state location and the config path. `gently init` creates a template only
when the config file is missing.

## Minimum configuration

Choose the base collector URL, without an endpoint suffix:

```toml
collector_url = "http://127.0.0.1:8787"
prefer_quic = false
```

For Cloudflare, use the HTTPS URL returned by `wrangler deploy`. The same URL
is used for export and query requests. Do not append `/v1/traces` or `/v1/query`;
the CLI adds those paths.

Supply the collector's shared bearer token as `GENTLY_TOKEN` through a secret
manager to the processes that need it. The CLI also supports a `token` config
key, but the setup guides keep credentials out of that file. Do not store vault
secrets in `.env`, `.dev.vars`, repository files or command arguments.

| Process or command | Needs the collector URL and token? |
| --- | --- |
| `gently hook` | No. It queues locally; a configured token allows detached export. |
| `gently init`, `gently status` | No. They use local configuration and state. |
| `gently export`, `gently export --watch` | Yes. |
| `gently traces`, `trace`, `spans`, `stats`, `whoami` | Yes. |
| MCP trace queries | Yes, in the MCP server process. |
| `gently waterfall` reading stdin | No. It does not load configuration. |

A watcher authenticates its own exports. It does not transfer its token to
other shells, hooks or MCP processes. After changing a token or watcher config,
restart the affected process. See [Local setup](local-collector.md) for the
hardware-backed helper and [Troubleshooting](../guides/troubleshooting.md) for
common authentication failures.

## File settings

Defaults are shown below; uncomment only the values you want to change:

```toml
collector_url = "https://gently-collector.<account>.workers.dev"

# prefer_quic = true
# outbox_cap = 10000
# export_batch = 512
# export_timeout_secs = 15
# query_timeout_secs = 30
# capture_raw_values = false
```

| Setting | Default | Meaning |
| --- | ---: | --- |
| `prefer_quic` | `true` | Prefer HTTP/3 for export, with a TCP fallback. Use `false` for localhost HTTP. |
| `outbox_cap` | `10000` | Limit applied when export drains the queue; excess oldest envelopes are dropped before sending. |
| `export_batch` | `512` | Queued envelopes coalesced into one export request. |
| `export_timeout_secs` | `15` | Per-request export timeout in seconds. |
| `query_timeout_secs` | `30` | Per-request CLI/MCP query timeout in seconds. |
| `capture_raw_values` | `false` | Store selected raw prompt, tool and assistant values locally in plaintext. |

One hook event queues one envelope, which can contain several spans. Queue
caps, pending counts and batch sizes count envelopes, not individual spans.
The outbox can exceed the cap before an exporter drains it, including when
hooks have no token.

## Environment overrides

| Variable | Behavior |
| --- | --- |
| `GENTLY_COLLECTOR_URL` | Nonempty value overrides `collector_url`. |
| `GENTLY_TOKEN` | Nonempty value overrides `token`. |
| `GENTLY_STATE_DIR` | Selects the config and state directory. |
| `GENTLY_QUERY_TIMEOUT_SECS` | Valid unsigned integer overrides `query_timeout_secs`; invalid values fall back to the file/default. |
| `GENTLY_CAPTURE_RAW_VALUES` | `1`/`true` enables capture; `0`/`false` disables it. Other values are errors. |
| `GENTLY_DEBUG=1` | Saves full hook payloads only when raw capture is also enabled. |
| `GENTLY_RESOLVE_LOCAL_SHA_RAW_VALUES=1` | Adds matching previously captured raw values to CLI/MCP query results. |

Capture and resolution are independent and off by default. Disabling capture
stops new writes; it does not delete previously stored raw values. Register MCP
resolution only deliberately with `--resolve-local-raw-values`. Running init
again without that option removes the registered resolution setting.
See [Security and privacy](../concepts/security-and-privacy.md).

## Local files

Paths below are relative to the state directory:

| Path | Contents |
| --- | --- |
| `config.toml` | Collector and operational settings. |
| `state.db` | Queued envelopes, span tracking, counters, exporter health and quarantine; raw values only when capture was enabled. |
| `export.log`, `hook.log` | Diagnostics; files over 5 MiB are rotated at process startup. A long-running watcher can grow beyond that threshold. |
| `export.lock` | Exporter singleton lock. |
| `raw/<harness>/*.jsonl` | Full hook payloads with both debug and raw capture enabled. |

On Unix, the state directory uses `0700`; application files and SQLite sidecars
use `0600`. Other platforms need an appropriate owner-only native ACL.
Use an OS-provided memory-backed directory only if you accept losing unsent
spans on shutdown. This changes local storage, not the collector's retention.
