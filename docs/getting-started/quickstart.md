# Quick start

Install the CLI, choose a collector, configure an agent, then capture and inspect
a session. Raw content capture and MCP raw-value resolution stay off throughout
this guide.

## 1. Install the CLI

You need Git, Rust and Cargo, plus Node.js and npm for the collector. The local
launcher additionally needs Python 3 and an inherited token from your secret
provider, as described in [local setup](local-collector.md).

```sh
git clone https://github.com/JamieAP/gently.git
cd gently
cargo install --path crates/gently-cli --locked
gently --version
```

If the shell cannot find `gently`, add Cargo's binary directory to `PATH`
(normally `~/.cargo/bin`). Run the same install command after updating the source.

## 2. Choose a collector

Export and query commands need a running collector and a host credential enrolled for
the configured tenant.
Hooks can queue events before either is available.

### Local collector

Follow [local setup](local-collector.md) to start a localhost Worker, local D1
and an export watcher with an inherited token. The separate macOS
`agent-secrets` helper is an optional foreground wrapper. No Cloudflare login or cloud database is
needed. Use `http://127.0.0.1:8787` as the collector URL.

### Cloudflare collector

You need a Cloudflare account with Wrangler authenticated for it. Install the
Worker dependencies and create its database:

```sh
cd worker
npm ci
npx wrangler d1 create gently
```

Put the returned `database_id` in `worker/wrangler.toml`, replacing
`REPLACE_AFTER_CREATE`. From the `worker` directory, initialize the database,
set the host authorization JSON through a private secret-provider workflow, and deploy:

```sh
npx wrangler d1 execute gently --remote --file schema.sql
npx wrangler secret put GENTLY_HOSTS
npx wrangler deploy
cd ..
```

The Worker secret is a JSON array with this shape; replace the public template
placeholder through your provider's private workflow:

```json
[{"token":"HOST_CREDENTIAL_FROM_PROVIDER","tenant_id":"personal","device_id":"mac-main","capabilities":["ingest","read"]}]
```

Use distinct credentials for each host. A capture/export-only host gets
`["ingest"]`; a reader gets `["read"]`. Each record names exactly one tenant
and device. Rotate or revoke records by updating the secret; there is no
built-in credential expiry or Access provisioning.

Keep the deployed URL. Export/query processes inherit their own host credential
as `GENTLY_TOKEN` from the provider. Set the matching `tenant_id` and `device_id`
in Gently configuration. Keep credentials out of repository files, argument
lists and shell history. See [configuration](configuration.md).

## 3. Configure an agent

Choose the integration you use, or initialize both:

```sh
gently init --claude
gently init --codex
```

Initialization preserves existing settings and can be run again. It installs
hook commands, registers `gently mcp`, and creates the Gently config only if it
is missing. The [CLI reference](../reference/cli.md) lists the affected files.
Codex users must also review and trust the hook entries inside Codex.

Set the collector URL in `~/.gently/config.toml`:

```toml
collector_url = "http://127.0.0.1:8787"
prefer_quic = false
tenant_id = "personal"
device_id = "local"
```

For Cloudflare, replace the URL with the deployed HTTPS URL and use the device
ID enrolled for your credential. The
`prefer_quic = false` setting is suitable for localhost; remote export can
prefer HTTP/3 with a TCP fallback. Omit that line to use the default.
If you use `GENTLY_STATE_DIR`, the config belongs in that directory instead.

Restart the agent so it loads the hooks and MCP registration, then run a short
task that uses a tool. Hooks with a token can start a detached exporter. Hooks
without a token only queue locally; a token-bearing `gently export --watch`
process can drain their queue. The local collector launcher starts that watcher
for you. The local launchers enable `--serve-queries` on Unix, allowing tokenless
CLI/MCP processes to delegate read-only queries through the private watcher
socket. The token remains in the watcher.

## 4. Verify capture and export

```sh
gently status
```

This reads local health without contacting the collector or needing a token.
After generating activity, look for a recent `last_success` and a draining
`pending (outbox)` count. `last_success = never` or a growing queue means you
should check the URL, watcher and token before expecting query results.

In a process that receives `GENTLY_TOKEN` from your secret manager:

```sh
gently traces
gently trace TRACE_ID --waterfall
gently spans --tool-name Bash
gently stats
```

Replace `TRACE_ID` with an ID from `gently traces`. These commands query the
collector, rather than the local outbox. For the local helper, one concrete
example is `agent-secrets run default -- gently traces`.

The waterfall shows timing, parent relationships and a diagnostic integrity
summary. It is part of the Rust binary. To render saved span JSON without a
collector, token or Python:

```sh
gently waterfall < trace.json
```

Use [Querying and MCP](../guides/querying-and-mcp.md) for filters, JSON piping and
agent tools. If capture or export is missing, follow
[Troubleshooting](../guides/troubleshooting.md).

## Keep raw capture deliberate

Ordinary traces contain byte lengths and identifying metadata. Encrypted raw
capture, ciphertext cloud sync and reader resolution are separate opt-ins.
None is needed to complete this guide. Use the
[enrollment guide](../guides/encrypted-raw-values.md) before enabling them.
Read [Security and privacy](../concepts/security-and-privacy.md) before enabling
either, including for hook diagnosis.
