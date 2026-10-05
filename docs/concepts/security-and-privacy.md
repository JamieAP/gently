# Security and privacy

Gently limits exported content fields to digests and byte lengths, but a trace
still contains identifying metadata. Decide what may be captured, where it may
be sent and who may query it before enabling the hooks.

## What is stored where

| Location | Contents |
| --- | --- |
| Collector | Span IDs, names, timings, statuses, tool/use IDs, context and identifying metadata, plus truncated content digests and byte lengths. |
| Local `state.db` | Pending envelopes, open-span bookkeeping, counters, quarantine and raw values when capture was enabled. |
| Local debug files | Full hook payloads when both raw capture and debug capture are enabled. |
| Local logs | Hook and export diagnostics. |

Content digests use the first 8 bytes of SHA-256. They are fingerprints, not
encryption or proof of anonymity. Low-entropy values can be guessed and matched.
Byte lengths and repeated fingerprints can also reveal patterns.

Working directories, hostnames, session/agent IDs, transcript paths, harness and
version are exported as metadata. Model, source, effort, permission mode and
agent type may also be included. Paths and activity patterns can identify
projects or people even when prompt and tool values are not exported verbatim.
Use a collector you control and are permitted to send that metadata to.

## Capture and resolution are separate

Both options are off by default and apply to different processes:

| Option | Set on | Effect |
| --- | --- | --- |
| `capture_raw_values = true` or `GENTLY_CAPTURE_RAW_VALUES=1` | Hook process | Store selected prompt, tool input/response and assistant values in local SQLite. |
| `GENTLY_RESOLVE_LOCAL_SHA_RAW_VALUES=1` | Query or MCP process | Add matching retained local raw values to supported query results. |

Raw values are plaintext and can include credentials, commands, file contents
or confidential material. They have no automatic retention limit. Turning off
capture stops new raw-value writes; it does not erase existing values, sidecars,
debug files or backups.

Resolution does not enable capture. It looks up values already stored locally
and does not add them to the export outbox. Resolved CLI output or MCP results
can expose that content to readers; inside an agent, it can enter the agent's
model-provider context.

Normal `gently init --claude` and `gently init --codex` do not enable resolution.
The `--resolve-local-raw-values` install option sets it for the registered MCP
server. Running init again without the option removes the registered setting;
an independently inherited environment flag can still enable it. See
[Querying and MCP](../guides/querying-and-mcp.md) for the affected results.

### Debug capture

`GENTLY_DEBUG=1` writes payload JSONL under
`~/.gently/raw/<harness>/<Event>.jsonl` only when raw capture is also enabled.
This can retain more content than the selected fields stored in SQLite. Debug
capture does not dump the whole process environment. Files remain after the
flags are disabled, so review their contents and retention after diagnosis.

## Local file protection

On Unix, Gently's private-file helpers create or restrict application/harness
state directories to `0700` and managed files to `0600`. Those files include
config, SQLite and sidecars, raw capture, logs, locks and updated harness config.
The helpers reject final-path symlinks and, on Unix, files owned by another user
or with multiple hardlinks before changing them.

These permissions do not encrypt content or protect it from the same user,
privileged processes or an agent allowed to read the files. Copies and backups
have their own permissions. Other platforms do not apply these Unix modes;
configure suitable owner-only ACLs separately. Keep local state and logs within
the access boundary appropriate for their contents.

## Collector access and transport

The Worker requires `Authorization: Bearer <GENTLY_TOKEN>` for its routes. The
single shared token grants read and write access to all traces; there is no
per-user or per-trace authorization. Anyone holding it can query the collector.

For a remote deployment, configure the token as a Worker secret and provide the
same value to authorized local processes. Keep it out of source-controlled
files, command arguments and shared logs. HTTPS protects remote transport.
Local development uses HTTP on loopback; do not treat that setup as a protected
remote endpoint. The project implements no tenant separation, mTLS, IP allowlist
or automatic trace retention.

Authentication failure stops export without quarantining the rejected send.
Export capacity trimming can already have removed older queued rows before that
request; see [Reliability](reliability.md#capacity-and-retention).

## Local launchers and process credentials

The bundled collector/export launcher scripts call a separately installed
`agent-secrets` helper. Secret storage and hardware unlock belong to that helper,
not the Rust CLI. Its installation and supported platforms are separate from
Gently; see [Local collector setup](../getting-started/local-collector.md).

A foreground launcher can pass a token to the Worker and export watcher through
process environments. Those processes then hold the plaintext token. Background
hooks do not initiate hardware unlock and can queue without a token.

The watcher's credentials are not transferred to separate CLI or MCP processes.
Each query process needs its own authorized configuration or inherited token.
Starting the collector and watcher does not establish authenticated desktop MCP
access. See [Configuration](../getting-started/configuration.md) for process
settings and [Architecture](architecture.md) for the data flow.
