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

For a local collector, open a separate terminal at the repository root. The
example token below is for local development only. Run the Worker in that
terminal while using the CLI in another:

```bash
cd worker
npm ci
echo 'GENTLY_TOKEN=dev-token' > .dev.vars
npx wrangler d1 execute gently --local --file schema.sql
npx wrangler dev                                  # http://127.0.0.1:8787
```

Use `http://127.0.0.1:8787` and the same local token in the CLI configuration.

## 2. Install the agent integration

```bash
cargo install --path crates/gently-cli           # installs the `gently` binary
gently init --claude                              # hooks + digest-only MCP queries
# or: gently init --codex                         # trust hooks inside Codex
```

`gently init --claude` is idempotent. It:

* adds hook entries to `~/.claude/settings.json` for the modeled events,
* registers the `gently` MCP server in `~/.claude.json`,
* scaffolds `~/.gently/config.toml`.

`gently init --codex` instead installs hooks and the MCP server in
`~/.codex/config.toml`. Trust the installed hooks inside Codex.

Set your collector + token in `~/.gently/config.toml` (see
[Configuration](configuration.md)), then **restart your agent session**. Hooks are read at session start. For Codex,
also trust the installed hooks inside Codex. Run a task to generate activity
before querying it.

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

Raw MCP resolution is disabled by default. To deliberately share locally stored
raw values with the calling agent, add `--resolve-local-raw-values` to init.
See [Security & privacy](../concepts/security-and-privacy.md).

The same query surface is exposed to the agent as MCP tools; see
[Querying & MCP](../guides/querying-and-mcp.md).

## Verify the hook schema (optional)

To check hook payloads for your installed agent version, run a session with
`GENTLY_DEBUG=1` and inspect the raw captures:

```bash
GENTLY_DEBUG=1 claude         # then look at ~/.gently/raw/<harness>/<Event>.jsonl
```
