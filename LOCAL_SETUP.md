# Local Gently collector

Run the collector on `http://127.0.0.1:8787` with the local D1 configuration.
This configuration creates no cloud database and requires no Cloudflare login.

## Prerequisites

Install the Gently CLI, Node.js, Python 3 and the worker dependencies:

```sh
cargo install --path crates/gently-cli --locked
cd worker
npm ci
npx wrangler d1 execute gently --local --config wrangler.local.toml --file schema.sql
cd ..
```

The schema command initializes the local D1 database. Use the separately installed
`agent-secrets` helper with a Secure Enclave-backed `default` profile containing
`GENTLY_TOKEN`. The helper and `claude-secure`/`codex-secure` launchers are local
machine tools; they are not installed by this repository.

The helper pins the default collector URL to `http://127.0.0.1:8787`. Store the token
only in the encrypted vault, never in `.env`, `.dev.vars`, command arguments or
Gently configuration. Enter values privately through the helper's interactive
terminal command. Unlock requires foreground user interaction: start the
launcher in Ghostty or another terminal and approve the macOS Touch ID/password
prompt. A desktop/background process cannot reliably perform this unlock.

## Start, stop, restart

From the repository root, start the collector and export watcher together;
leave the foreground terminal running:

```sh
./scripts/collector-local
```

Optionally link the launcher after cloning to `~/dev/gently`:

```sh
mkdir -p ~/.local/bin
ln -s "$HOME/dev/gently/scripts/collector-local" "$HOME/.local/bin/gently-collector"
gently-collector
```

The launcher resolves its own location, including symlinks, and uses Node.js from
`PATH`. It supplies `GENTLY_TOKEN` as the collector binding; child processes also
inherit the launcher's environment. It disables telemetry and disk diagnostic logs
and listens only on localhost.
It unlocks once, then passes the token only through process environments to the
Worker and `gently export --watch --interval-secs 2`. The watcher drains spans
queued by desktop hooks without prompting again.

Stop the collector and watcher with Ctrl+C in that terminal. Before upgrading
or restarting, stop the old instance first, then run `gently-collector` or
`./scripts/collector-local` again. It is not a login service; start it again
after reboot. Hooks buffer spans locally while the collector is unavailable.

To attach a watcher to a collector that is already running, leave that collector
in place and start `./scripts/export-local` in a foreground terminal. The
exporter-only launcher unlocks once and runs until Ctrl+C. Optionally link it as
`gently-exporter`:

```sh
ln -s "$HOME/dev/gently/scripts/export-local" "$HOME/.local/bin/gently-exporter"
gently-exporter
```

## Agent integration

Initialize Gently for both agents:

```sh
gently init --claude
gently init --codex
```

For terminal sessions, `claude-secure` or `codex-secure` can unlock once and pass
the token to hooks and MCP. Desktop hooks without a token still queue locally
and do not spawn exporters; the collector launcher's watcher drains that queue.
In Codex, run
`/hooks` once and review/trust the Gently entries. Both agents use the Gently MCP
server and hook commands installed by `init`. Existing settings are preserved;
running initialization again is idempotent.

For local tracing, use this collector configuration in `~/.gently/config.toml`:

```toml
collector_url = "http://127.0.0.1:8787"
prefer_quic = false
```

Keep the token out of that file. Raw prompt/tool/assistant capture is off by
default. Enable it only with `capture_raw_values = true` or
`GENTLY_CAPTURE_RAW_VALUES=1`. Debug payload files additionally require
`GENTLY_DEBUG=1`; no environment snapshots are generated. Initialization leaves
MCP raw-value resolution disabled unless explicitly requested, independently of
capture. Disabling capture does not remove existing values.

## Verify and query

```sh
gently status
agent-secrets run default -- gently traces
```

`status` reads local metadata without unlocking the vault: queued envelopes,
last successful export and errors. `traces`, `trace`, `spans` and `stats` query the
local collector and require a token. Unauthenticated requests receive 401.

Local state lives in `~/.gently/state.db` and `worker/.wrangler/state`. The outbox
contains metadata and content digests; raw values are stored separately in the
local state database only when capture was enabled. These files are ignored by
Git. Authentication rejection stops export immediately with the queue retained;
correct the token and restart the launcher. Query commands also require a
token-bearing foreground process. Starting the launcher does not prove the queue
has drained: verify `gently status` and collector query results.
The token stays encrypted in the hardware-bound vault. Configure an independent
recovery recipient before storing irreplaceable credentials.
