# Quick start

Install the CLI, choose a collector, configure an agent, then capture and inspect
a session. Raw content capture and MCP raw-value resolution stay off throughout
this guide.

## 1. Install the CLI

You need Git, Rust and Cargo, plus Node.js and npm for the collector. The local
launcher additionally needs Python 3 and the external helper described in
[Local setup](local-collector.md).

```sh
git clone https://github.com/JamieAP/gently.git
cd gently
cargo install --path crates/gently-cli --locked
gently --version
```

If the shell cannot find `gently`, add Cargo's binary directory to `PATH`
(normally `~/.cargo/bin`). Run the same install command after updating the source.

## 2. Choose a collector

Export and query commands need a running collector and its shared bearer token.
Hooks can queue events before either is available.

### Local collector

Follow [Local setup](local-collector.md) if you have the separate
`agent-secrets` helper. It starts a localhost Worker, local D1 and an export
watcher after one foreground unlock. No Cloudflare login or cloud database is
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
set the token through Wrangler's private interactive prompt, and deploy:

```sh
npx wrangler d1 execute gently --remote --file schema.sql
npx wrangler secret put GENTLY_TOKEN
npx wrangler deploy
cd ..
```

Keep the deployed URL. Supply the same token as `GENTLY_TOKEN` to export and
query processes through your secret manager. Keep it out of repository files,
command arguments and shell history. See [Configuration](configuration.md).

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
```

For Cloudflare, replace the URL with the deployed HTTPS URL. The
`prefer_quic = false` setting is suitable for localhost; remote export can
prefer HTTP/3 with a TCP fallback. Omit that line to use the default.
If you use `GENTLY_STATE_DIR`, the config belongs in that directory instead.

Restart the agent so it loads the hooks and MCP registration, then run a short
task that uses a tool. Hooks with a token can start a detached exporter. Hooks
without a token only queue locally; a token-bearing `gently export --watch`
process can drain their queue. The local collector launcher starts that watcher
for you. Neither approach supplies a token to other CLI or MCP query processes.

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

Ordinary traces contain digests and identifying metadata, rather than raw
prompt or tool content. Capturing local raw content and resolving it into query
results are separate opt-ins. Neither is needed to complete this guide.
Read [Security and privacy](../concepts/security-and-privacy.md) before enabling
either, including for hook diagnosis.
