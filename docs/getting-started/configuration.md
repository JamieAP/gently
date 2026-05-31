# Configuration

gently reads `~/.gently/config.toml`, with environment variables overriding the
essentials. `gently init` scaffolds a documented template.

## The one knob that matters

`collector_url` selects the store. **Both export and queries follow it** -
there's no separate read/write target.

* **Cloudflare D1 (default):** `https://gently-collector.<account>.workers.dev`
* **Local `wrangler dev`:** `http://127.0.0.1:8787`

## `config.toml`

A minimal config is just the two essentials; everything else has a sane default.

```toml
# Required - the collector and its shared bearer token.
collector_url = "https://gently-collector.<account>.workers.dev"
token         = "…"

# Optional tunables (defaults shown):
# prefer_quic = true          # prefer HTTP/3 (QUIC) for export, fall back to HTTP/2
# outbox_cap  = 10000         # max buffered spans before the oldest are dropped
# export_batch = 512          # spans coalesced into one export request
# export_timeout_secs = 15    # per-request export timeout
# query_timeout_secs  = 30    # per-request query / MCP timeout
```

## Environment overrides

For the essentials and the state location:

| Variable | Overrides |
|---|---|
| `GENTLY_COLLECTOR_URL` | `collector_url` |
| `GENTLY_TOKEN` | `token` |
| `GENTLY_STATE_DIR` | the state directory (default `~/.gently`) |
| `GENTLY_DEBUG` (any value) | also write raw hook payloads to `~/.gently/raw/` |

## Where things live

`GENTLY_STATE_DIR` (default `~/.gently`) holds the local state:

| Path | Contents |
|---|---|
| `config.toml` | collector URL + token |
| `state.db` | the SQLite outbox, open spans, turn counters, health, quarantine |
| `export.log` / `hook.log` | diagnostics (rotated at 5 MB) |
| `export.lock` | exporter singleton lock |
| `raw/*.jsonl` | raw hook payloads - only when `GENTLY_DEBUG` is set (any value) |

> Point `GENTLY_STATE_DIR` at a tmpfs (e.g. `/dev/shm`) to keep local state off
> persistent disk, at the cost of losing buffered-but-unsent spans on reboot.

## Token handling

The token is a shared bearer secret. Keep `config.toml` at `0600`. It is never logged, and never placed in `wrangler.toml` - on the Worker
it's a Cloudflare secret (`wrangler secret put GENTLY_TOKEN`).
