# Quick start

Three steps: deploy the collector, install the agent integration, query.

## 1. Deploy the collector

The collector is a Cloudflare Worker backed by a D1 database. You need a
Cloudflare account and `wrangler` logged in.

```bash
cd worker
npm install
wrangler d1 create gently                       # paste database_id into wrangler.toml
wrangler d1 execute gently --remote --file schema.sql
wrangler secret put GENTLY_TOKEN                 # a shared bearer token
wrangler deploy                                  # → https://gently-collector.<acct>.workers.dev
```

Prefer to stay local? Run a local collector instead - same code path, a local D1:

```bash
echo 'GENTLY_TOKEN=dev-token' > worker/.dev.vars
wrangler d1 execute gently --local --file schema.sql
wrangler dev                                     # http://127.0.0.1:8787
```

## 2. Install the agent integration

```bash
cargo install --path crates/gently-cli           # installs the `gently` binary
gently init --claude                              # hooks + MCP server + ~/.gently/config.toml
```

`gently init` is idempotent. It:

* adds hook entries to `~/.claude/settings.json` for the modeled events,
* registers the `gently` MCP server in `~/.claude.json`,
* scaffolds `~/.gently/config.toml`.

Set your collector + token in `~/.gently/config.toml` (see
[Configuration](configuration.md)), then **restart your Claude Code session** -
hooks are read at session start. Spans now flow on every tool call.

## 3. Query

From the shell:

```bash
gently traces                  # recent sessions
gently trace <trace_id>        # the full span tree
gently spans --tool-name Bash  # filter spans
gently stats                   # per-tool rollups
gently status                  # local exporter health + queue depth
```

Render a waterfall (the display below is synthetic):

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

The same query surface is exposed to the agent as MCP tools - see
[Querying & MCP](../guides/querying-and-mcp.md).

## Verify the hook schema (optional)

The Claude Code hook payloads are confirmed empirically, not assumed. Run a
session with `GENTLY_DEBUG=1` and inspect the raw captures:

```bash
GENTLY_DEBUG=1 claude         # then look at ~/.gently/raw/<Event>.jsonl
```
