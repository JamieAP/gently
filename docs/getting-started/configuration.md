# Configuration

Gently reads `~/.gently/config.toml`; environment overrides apply to the collector,
token, state directory and explicitly enabled capture/query features.
`gently init` scaffolds a template without replacing an existing file.

## Collector and authentication

`collector_url` selects both the export and query target:

* Cloudflare D1: `https://gently-collector.<account>.workers.dev`
* Local Wrangler: `http://127.0.0.1:8787`

Hooks can record locally without a token. Export and queries require the shared
bearer token. Prefer supplying `GENTLY_TOKEN` from your secret manager to the
processes that need it; the `token` config key remains supported for private
local configurations. Never put a token in repository files or command arguments.
For the hardware-backed local setup, see [Local setup](../../LOCAL_SETUP.md).

## `config.toml`

```toml
collector_url = "https://gently-collector.<account>.workers.dev"

# Optional tunables (defaults shown):
# prefer_quic = true          # HTTP/3 (QUIC), with HTTP/2 fallback
# outbox_cap = 10000          # buffered OTLP envelopes; oldest dropped at the cap
# export_batch = 512          # outbox envelopes coalesced into one request
# export_timeout_secs = 15    # per-request export timeout
# query_timeout_secs = 30     # per-request query / MCP timeout
# capture_raw_values = false  # local plaintext prompt/tool/assistant capture
```

Each hook event queues one envelope containing all spans it emits. A row can
therefore contain several spans; queue caps, pending counts and export batch
sizes count envelopes rather than individual spans.

## Environment overrides

| Variable | Effect |
|---|---|
| `GENTLY_COLLECTOR_URL` | Overrides `collector_url` |
| `GENTLY_TOKEN` | Overrides `token` |
| `GENTLY_STATE_DIR` | State directory (default `~/.gently`) |
| `GENTLY_QUERY_TIMEOUT_SECS` | Overrides `query_timeout_secs` |
| `GENTLY_CAPTURE_RAW_VALUES=1` | Enables local raw-value capture |
| `GENTLY_DEBUG=1` | Saves raw hook payloads only when raw capture is also enabled |
| `GENTLY_RESOLVE_LOCAL_SHA_RAW_VALUES=1` | Adds matching previously captured raw values to CLI/MCP query results |

Capture and resolution are independent opt-ins. Neither is enabled by default.
Turning capture off stops new writes but retains existing raw values. Debug
capture never generates a process-environment snapshot.

## Where things live

The state directory holds these files:

| Path | Contents |
|---|---|
| `config.toml` | Collector and operational settings (`0600`) |
| `state.db` | Outbox envelopes, open spans, turn counters, health, quarantine; raw values only when capture was enabled |
| `export.log` / `hook.log` | Diagnostics, rotated at 5 MB |
| `export.lock` | Exporter singleton lock |
| `raw/<harness>/*.jsonl` | Hook payloads only with both debug and raw-capture opt-ins |

Point `GENTLY_STATE_DIR` at a tmpfs such as `/dev/shm` to keep local state off
persistent disk, at the cost of losing unsent spans on reboot.

## Token and raw-value handling

On Unix, config/state files and SQLite sidecars, logs, locks and debug files use
`0600`; the state directory uses `0700`. Other platforms require an owner-only
native ACL. Configure the Worker token with `wrangler secret put GENTLY_TOKEN`,
not in `wrangler.toml`.

A Secure Enclave unlock requires foreground user interaction. Desktop/background
hooks without an inherited token only queue; run a foreground-unlocked export
watcher to drain them. Queries also need a token-bearing process. MCP installation
enables raw resolution only with `--resolve-local-raw-values`, and resolved
content may be shared with the calling agent's provider. See
[Security & privacy](../concepts/security-and-privacy.md).
