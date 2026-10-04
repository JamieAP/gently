# Security & privacy

## Exported metadata and local raw content

Normal exports contain span IDs, names, timings, status, tool names/use IDs,
permission mode, and truncated SHA-256 digests plus byte lengths of prompt and
tool values. Raw prompt, command, file, and tool output values are excluded from
the export outbox. Digests are fingerprints rather than encryption and can be
guessed for low-entropy content.

Exports intentionally identify the working directory, hostname, operating system,
session and agent IDs, harness/version, and sometimes model, session source,
effort level, close reason, agent type, and agent transcript path. This metadata
can reveal projects, local paths, activity patterns, and the tools/models used.
Only export traces to a collector you control and are allowed to use.

Selected raw prompts, tool inputs/responses, and assistant messages are stored
locally in `~/.gently/state.db`, keyed by digest. They can include credentials or
confidential source material from a session. This storage is enabled in the hook
and has no automatic retention limit; protecting or deleting local state is an
operator responsibility.

CLI/MCP queries return collector digest attributes by default. Explicitly set
`GENTLY_RESOLVE_LOCAL_SHA_RAW_VALUES=1` on a query process to add matching local
raw attributes. `gently init --claude` and `gently init --codex` leave this disabled.
The `--resolve-local-raw-values` install option enables it for the registered MCP
server; re-running init without the option removes that setting. If MCP is used
inside an agent, resolved values may enter that agent's model-provider context.

Setting `GENTLY_DEBUG` to any value additionally saves full hook payloads to
`~/.gently/raw/<harness>/<Event>.jsonl` and an environment snapshot at
`raw/<harness>/env.json`. Environment keys containing KEY, TOKEN, SECRET,
PASSWORD, AUTH, or CREDENTIAL are masked; this name-based list cannot identify
all sensitive values. Debug capture is disabled by default and has no retention
limit. Disable it after diagnosis and review/delete captured files as needed.

## Local files

On Unix, gently creates or restricts application/harness state directories to
`0700` and config, SQLite/database sidecars, raw capture, logs, locks, and updated
harness config files to `0600`. Existing files opened through these paths are
restricted before use; final-path symlinks are rejected. Unix helpers also refuse
files owned by another user or with multiple hardlinks before changing their
permissions or contents. Local content is
plaintext, and backups made before restriction are not changed. These controls
do not protect against the same user, privileged processes, or an agent allowed
to read the files. On other platforms the native inherited ACL applies; configure
an owner-only ACL separately because Unix modes are not enforced there.

## Transport and access

Remote collectors should use HTTPS. HTTP is available for local development.
The Worker requires `Authorization: Bearer <GENTLY_TOKEN>` on its routes; the
single shared token grants access to all traces. Configure it as a Cloudflare
secret and in your private local config/environment, not in `wrangler.toml`.
The repository contains a placeholder D1 database identifier to replace when
you create your own database.

The collector relies on Cloudflare's service controls for data at rest. This
project does not implement tenant separation, an IP allowlist, mTLS, or automatic
retention. The public Worker endpoint is protected by the shared bearer token.
