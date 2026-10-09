# Local collector

Run the supplied Worker and D1 database at `http://127.0.0.1:8787`, with a
foreground exporter that drains the local outbox. This setup creates no cloud
database and requires no Cloudflare login.

## Before you start

You need Rust and Cargo, Node.js and npm, Python 3, and a secret provider that
supplies `GENTLY_TOKEN` to a child process. The launchers are provider-neutral
and work with inherited credentials on Macs and Linux hosts. The optional
macOS `agent-secrets` helper, its Secure Enclave vault, and `claude-secure` or
`codex-secure` launchers are separately installed machine tools.

Use the provider's private terminal workflow to configure the token. Keep it
out of configuration files, `.env`, `.dev.vars`, command arguments and shell
history. Hardware unlock belongs to a foreground provider invocation; hooks
and background exporters never request it.

## 1. Install and initialize local D1

From the repository root:

```sh
cargo install --path crates/gently-cli --locked
cd worker
npm ci --ignore-scripts
npx wrangler d1 execute gently --local --config wrangler.local.toml --file schema.sql
cd ..
```

The schema command initializes local D1. The local Wrangler configuration does
not need a cloud database ID. State from early development builds,
before the tenant/encrypted schema, must be explicitly reset; there is no
migration from it.

## 2. Configure capture

```sh
gently init --claude
# For Codex: gently init --codex
```

Set the following in `~/.gently/config.toml`, or the directory selected by
`GENTLY_STATE_DIR`:

```toml
collector_url = "http://127.0.0.1:8787"
prefer_quic = false
tenant_id = "personal"
device_id = "local"
```

Use the same tenant, device and state directory for hooks and the exporter.
The launchers resolve the same file settings and environment overrides as the
CLI; `personal`/`local` apply only when neither specifies a namespace. Restart
the agent, and review/trust installed Codex hooks inside
Codex. Raw capture, ciphertext sync and reader resolution remain off.

## 3. Preflight and start

Check dependencies before requesting any credential unlock:

```sh
./scripts/collector-local --check
./scripts/export-local --check
```

These checks validate dependencies, configuration, public capture policy,
configured reader paths and an existing state schema. They need no token,
never unlock an identity and never call a secret provider. In a foreground
terminal that already inherits `GENTLY_TOKEN`, start:

```sh
./scripts/collector-local
```

For the separately installed macOS helper, the equivalent explicit wrapper is:

```sh
./scripts/collector-local --check
agent-secrets run default -- ./scripts/collector-local
```

The provider unlocks once, then Gently starts the Worker and
`gently export --watch`. The supervisor creates the Worker's `GENTLY_HOSTS`
authorization map in memory from the inherited token and tenant/device IDs;
it sets an owner-only creation mask and hardens local Wrangler directories and
files to `0700`/`0600`, including existing D1 databases, without releasing SQLite locks;
it removes `GENTLY_TOKEN` from the Worker environment. No plaintext secret file
or token argument is created. The Worker binds to loopback; the launcher uses
Wrangler's API to disable its DevTools inspector, remote bindings, telemetry
and diagnostic output. Either child's exit stops both owned process
groups, including descendants of a parent that has already exited.

Leave the terminal open. Stop both services with Ctrl+C, and stop an old
instance before starting another. This is a foreground job, not a login
service. Hooks continue to queue while it is unavailable.

If the collector is already running, use an inherited token with:

```sh
./scripts/export-local
```

Or, after its preflight, wrap that command with your provider. The exporter
must receive the same local tenant/device credential as the collector.

## 4. Verify delivery and queries

Run a short tool-using agent task, then check local health:

```sh
gently status
```

Look for a recent `last_success` and a draining `pending (outbox)` count. A
running launcher alone does not prove delivery. While the local exporter serves
queries, a separate CLI or MCP process can use its same-user, tenant/device-scoped
Unix socket without inheriting the token:

```sh
gently traces
gently trace TRACE_ID --waterfall
```

Replace `TRACE_ID` with an ID from the trace list. For direct HTTP access instead,
supply a read credential through your provider. Tokenless desktop hooks queue
for the watcher; desktop MCP uses the local query broker. Claude Chat/Cowork
integration is outside the coding-agent scope.

## State and failures

Gently runtime state defaults to
`~/.gently/tenants/personal/devices/local/state.db`; local D1 is under
`worker/.wrangler/state`. Both are ignored by Git. Metadata still includes paths,
host information and activity patterns. Optional selected raw fields are
encrypted before persistence; plaintext full-payload debug capture is removed.
Disabling capture does not erase
retained ciphertext, old plaintext development files or backups.

Authentication rejection retains pending rows and stops export. Correct the
credential through the provider and restart. Unreachable collectors are
retried; malformed envelopes can be quarantined. See
[troubleshooting](../guides/troubleshooting.md),
[configuration](configuration.md), and
[encrypted raw enrollment](../guides/encrypted-raw-values.md).

Gently never installs, moves or resets the separate credential vault. Manage
its recovery through the provider's own workflow.
