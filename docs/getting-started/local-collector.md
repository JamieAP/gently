# Local collector

Run the supplied Worker and D1 database at `http://127.0.0.1:8787`, with a
foreground exporter that drains the local outbox. This setup creates no cloud
database and does not require Cloudflare login.

## Before you start

You need Rust and Cargo, Node.js and npm, Python 3, and a separately installed
`agent-secrets` helper. The supplied launchers use that helper's `default`
profile to receive `GENTLY_TOKEN` after a foreground unlock. The helper, vault,
`claude-secure` and `codex-secure` launchers are local machine tools; this
repository does not install them.

If you do not have that helper, use the
[Cloudflare quick start](quickstart.md#cloudflare-collector).
Gently's collector and query API use a bearer token, but the supplied local
launch scripts specifically expect `agent-secrets`.

Store the token in the encrypted vault through its private interactive
terminal workflow. Do not print it or put it in `.env`, `.dev.vars`, command
arguments, shell history or Gently configuration. Unlocking a hardware-backed
identity requires foreground interaction, such as a macOS Touch ID/password
prompt. Hooks and desktop/background processes do not perform that unlock.

## 1. Install and initialize local D1

From the repository root:

```sh
cargo install --path crates/gently-cli --locked
cd worker
npm ci
npx wrangler d1 execute gently --local --config wrangler.local.toml --file schema.sql
cd ..
```

The schema command initializes local D1. `wrangler.local.toml` is separate from
the remote configuration and does not need a cloud database ID.

## 2. Configure capture

Install the integration you use. Init creates the state directory and config
file if they are missing:

```sh
gently init --claude
# For Codex: gently init --codex
```

Then set the URL in `~/.gently/config.toml`, or in the directory selected by
`GENTLY_STATE_DIR`:

```toml
collector_url = "http://127.0.0.1:8787"
prefer_quic = false
```

You can initialize both. Existing settings are preserved; running init again is
idempotent. Restart the agent afterward. In Codex, review/trust the installed
hooks inside Codex. Use the same state directory for hooks, the watcher and
local health checks.

Raw prompt/tool/assistant capture and registered MCP raw-value resolution remain
off. Neither is required to inspect ordinary traces.

## 3. Start the collector and exporter

Leave a foreground terminal running:

```sh
./scripts/collector-local
```

Approve the helper's unlock prompt once. The launcher starts the Worker and
`gently export --watch` with the inherited token, binds the Worker to localhost,
and disables Wrangler telemetry and disk diagnostic logs. If either child
exits, its companion is stopped as well.

Stop both with Ctrl+C. Stop an old instance before starting another. This is a
foreground job, not a login service; start it again after reboot. While it is
unavailable, hooks continue to queue locally. The queue cap is applied when
export drains the outbox, rather than when tokenless hooks enqueue events.

If a collector is already running, attach only the export watcher:

```sh
./scripts/export-local
```

This also unlocks in the foreground and runs until Ctrl+C. It expects the same
local URL and token as the collector.

### Optional shortcuts

From the repository root, link the launchers into a directory on `PATH`:

```sh
mkdir -p ~/.local/bin
ln -s "$PWD/scripts/collector-local" "$HOME/.local/bin/gently-collector"
ln -s "$PWD/scripts/export-local" "$HOME/.local/bin/gently-exporter"
```

Then run `gently-collector` or `gently-exporter`. If a link already exists,
review its target before replacing it. The collector launcher resolves its
location through symlinks; moving the checkout requires updating the links.

## 4. Generate activity and verify delivery

Start a new agent session and run a short tool-using task. Inspect local health:

```sh
gently status
```

Look for a recent `last_success` and a draining `pending (outbox)` count. A
running launcher alone does not prove delivery. Query the collector from a
separate process that receives the token:

```sh
agent-secrets run default -- gently traces
agent-secrets run default -- gently trace TRACE_ID --waterfall
```

Replace `TRACE_ID` with an ID from the trace list. `status` reads local state
without a token; trace queries contact the collector and require one.

For terminal agent sessions, the separate `claude-secure` or `codex-secure`
launchers can supply an inherited token to hooks and MCP. Desktop hooks can
queue without a token, and the watcher can export that queue. The watcher's
credentials do not authenticate a separately launched CLI or MCP server.
Authenticated desktop MCP access and Claude Chat/Cowork integration remain
incomplete.

## State and failures

Gently state defaults to `~/.gently/state.db`; local D1 state is under
`worker/.wrangler/state`. They are ignored by Git. Normal outbox records contain
digests and identifying metadata. Raw values are stored in plaintext only when
capture was enabled, and remain after it is disabled. Debug payload files need
both raw capture and debug opt-ins. There is no automatic retention.

Authentication rejection stops export with the queue retained. Correct the token
through the secret manager and restart the launcher. Unreachable collectors are
retried; malformed or unprocessable envelopes can be quarantined. Use
[Troubleshooting](../guides/troubleshooting.md) to interpret health fields and
[Security and privacy](../concepts/security-and-privacy.md) before enabling
raw capture or sharing state. Manage recovery of a hardware-bound vault in the
helper's own configuration; Gently does not back up its secrets.
