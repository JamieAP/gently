# Quick start

Deploy the collector, install the integration, then query a session. Start from
the repository root.

## 1. Deploy the collector

The collector is a Cloudflare Worker backed by a D1 database. You need a
Cloudflare account and `wrangler` logged in.

```bash
cd worker
npm ci
npx wrangler d1 create gently                       # paste database_id into wrangler.toml
npx wrangler d1 execute gently --remote --file schema.sql
npx wrangler secret put GENTLY_TOKEN                 # a shared bearer token
npx wrangler deploy                              # returns the collector URL
cd ..
```

Prefer to stay local? Follow [Local setup](../../LOCAL_SETUP.md) for a localhost
collector with local D1, a hardware-backed token, and an export watcher. It
does not require Cloudflare login or a plaintext token file. Use
`http://127.0.0.1:8787` as the collector URL.

## 2. Install the agent integration

```bash
cargo install --path crates/gently-cli --locked  # installs the `gently` binary
gently init --claude                              # hooks + digest-only MCP queries
# or: gently init --codex                         # trust hooks inside Codex
```

`gently init --claude` is idempotent. It:

* adds hook entries to `~/.claude/settings.json` for the modeled events,
* registers the `gently` MCP server in `~/.claude.json`,
* scaffolds `~/.gently/config.toml`.

`gently init --codex` instead installs hooks and the MCP server in
`~/.codex/config.toml`. Trust the installed hooks inside Codex.

Set `collector_url` in `~/.gently/config.toml` and supply `GENTLY_TOKEN` to export
and query processes (see [Configuration](configuration.md)), then restart your
agent session. Hooks record each event locally. Token-bearing hooks start a
detached exporter; hooks without a token can use a foreground-unlocked
`gently export --watch --interval-secs 2` process to drain their queue. The
watcher does not authenticate CLI or MCP queries. Run a task to generate
activity before querying it.

## 3. Query

From the shell:

```bash
gently traces                  # recent sessions
gently trace <trace_id>        # the full span tree
gently spans --tool-name Bash  # filter spans
gently stats                   # per-tool rollups
gently status                  # local exporter health + queue depth
```

Render a waterfall (the example below is synthetic, with millisecond units):

```bash
gently trace <trace_id> --json | python3 scripts/waterfall.py
```

```
      dur st  span        │timeline →                                              │
 1000.0ms ✓  session      │████████████████████████████████████████████████████████│
  900.0ms ✓    turn:1     │  ██████████████████████████████████████████████████████│
    20.0ms ✓      Bash     │               ██                                       │
   40.0ms ✓      Bash     │                          ████                          │
     2.0ms ✓      Bash     │                                              █         │
```

Raw capture and MCP resolution are disabled by default. To deliberately share
local raw values, separately enable capture using `capture_raw_values = true`
or `GENTLY_CAPTURE_RAW_VALUES=1`, then add `--resolve-local-raw-values` to init.
Existing captured values remain available after capture is disabled.
See [Security & privacy](../concepts/security-and-privacy.md).

The same query surface is exposed to the agent as MCP tools; see
[Querying & MCP](../guides/querying-and-mcp.md).

## Verify the hook schema (optional)

Adapters are checked against the current official hook schemas with synthetic
regressions; see [Harness hooks](../reference/hooks.md). If you need to inspect a
real session payload, explicitly enable both raw capture and debug capture:

```bash
GENTLY_CAPTURE_RAW_VALUES=1 GENTLY_DEBUG=1 claude
# Inspect ~/.gently/raw/<harness>/<Event>.jsonl; no environment snapshot is taken.
```
