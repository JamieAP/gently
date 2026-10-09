# Configuration

Gently loads `config.toml` from its state directory, then applies environment
overrides. The default directory is `~/.gently`; `GENTLY_STATE_DIR` changes both
the state location and the config path. `gently init` creates a template only
when the config file is missing.

## Minimum configuration

Choose the base collector URL, without an endpoint suffix, and a tenant/device
that matches the collector's credential enrollment:

```toml
collector_url = "http://127.0.0.1:8787"
prefer_quic = false
tenant_id = "personal"
device_id = "local"
```

For Cloudflare, use the deployed HTTPS URL. The CLI adds endpoint paths and the
`tenant_id` query parameter. Supply `GENTLY_TOKEN` through your secret provider
to export and query processes. Tokens are environment-only: a `token` setting
in the config is rejected. Keep credentials out of `.env`, `.dev.vars`,
repository files, command arguments and shell history.

| Process or command | Collector token required? |
| --- | --- |
| `gently hook` | No. It queues locally; an inherited token permits detached export. |
| `gently init`, `gently status`, `gently config`, `gently raw` | No. They operate on local configuration, state or keys. |
| `gently export`, `gently export --watch` | Yes, with ingest capability. |
| `gently traces`, `trace`, `spans`, `stats`, `whoami`, MCP queries | Yes, with read capability. |
| `gently waterfall` reading stdin | No. It does not load configuration. |

A watcher does not transfer its token to other shells, hooks or MCP processes.
Restart affected processes after changing credentials or configuration. See
[local setup](local-collector.md) for provider-neutral launchers.

## File settings

```toml
collector_url = "https://gently-collector.<account>.workers.dev"
tenant_id = "personal"
device_id = "mac-main"

# prefer_quic = true
# outbox_cap = 10000
# export_batch = 512
# export_timeout_secs = 15
# query_timeout_secs = 30
# capture_raw_values = false
# sync_raw_values = false
# resolve_raw_values = false
# raw_manifest = "/absolute/path/manifest.json"
# raw_trust = "/absolute/path/trust.json"
# raw_identity = "/absolute/path/reader.age"
```

| Setting | Default | Meaning |
| --- | --- | --- |
| `tenant_id` | `personal` | Namespace for capture, export and query; must match the authenticated host. |
| `device_id` | `local` | Capture host identifier; must match the upload credential's device. |
| `prefer_quic` | `true` | Prefer HTTP/3 export with a TCP fallback; use `false` for localhost HTTP. |
| `outbox_cap` | `10000` | 1–1000000 envelopes; trimming applies only with explicit `export --discard-oldest`. |
| `export_batch` | `512` | 1–4096 queued envelopes coalesced into one export request. |
| `export_timeout_secs` | `15` | Per-request export timeout, 1–3600 seconds. |
| `query_timeout_secs` | `30` | Per-request CLI/MCP query timeout, 1–3600 seconds. |
| `capture_raw_values` | `false` | Encrypt selected raw fields before storing them locally. |
| `sync_raw_values` | `false` | Upload retained ciphertext before draining metadata; needs no reader identity. |
| `resolve_raw_values` | `false` | Unlock a reader only for referenced ciphertext and hydrate raw fields in query results. |
| `raw_manifest` | unset | Owner-signed public reader enrollment manifest used by capture. |
| `raw_trust` | unset | Locally verified tenant owner key, minimum epoch and public manifest digest used by capture. |
| `raw_identity` | unset | Reader identity used only by explicit resolution or owner operations. |

One hook event queues one envelope, possibly containing several spans. Queue
caps and batches count envelopes; a tokenless queue can exceed the cap until an
exporter drains it. Ciphertext retention is separate from this cap: each
tenant/device database admits at most 64 MiB of encoded ciphertext envelopes,
excluding SQLite page/WAL overhead. Full storage skips new raw capture or
caching without evicting existing objects. Metadata capture continues.
Rejected raw objects remain encrypted in quarantine and do not block metadata;
inspect `gently status` and explicitly retry with
`gently export --retry-raw-quarantine` after correcting the collector issue.

## Environment overrides

| Variable | Behavior |
| --- | --- |
| `GENTLY_COLLECTOR_URL` | Nonempty value overrides `collector_url`. |
| `GENTLY_TOKEN` | Supplies the process's bearer credential; no persisted-token fallback. |
| `GENTLY_STATE_DIR` | Selects the config and state directory. |
| `GENTLY_TENANT_ID`, `GENTLY_DEVICE_ID` | Override the corresponding namespace identifiers. |
| `GENTLY_QUERY_TIMEOUT_SECS` | Valid unsigned integer overrides query timeout. |
| `GENTLY_CAPTURE_RAW_VALUES` | `1`/`true` enables encrypted capture; `0`/`false` disables it. |
| `GENTLY_SYNC_RAW_VALUES` | Enables or disables ciphertext upload using the same boolean values. |
| `GENTLY_RESOLVE_RAW_VALUES` | Enables or disables reader-side resolution using the same boolean values. |
| `GENTLY_RAW_MANIFEST`, `GENTLY_RAW_TRUST`, `GENTLY_RAW_IDENTITY` | Override the corresponding file paths. |

Capture, cloud sync and resolution are independent and disabled by default.
Invalid boolean/numeric values and relative raw paths are configuration errors.
Raw paths must be absolute; expand `~` before putting a path in TOML.
Register MCP resolution
explicitly with `gently init --claude --resolve-raw-values` or the equivalent
Codex command. Reinstalling without that option removes the registration flag;
file settings or independently inherited environment settings still apply.
See [encrypted raw setup](../guides/encrypted-raw-values.md).

`gently config --json` prints only the resolved collector URL, state directory,
tenant and device. `gently config --check` verifies enabled public capture policy,
the metadata of a configured reader path and an existing state schema. It needs
no token and never reads or unlocks a reader identity. Metadata queries and MCP
initialization remain available when an opted-in private reader is unavailable.

## Local files

`config.toml` stays at the state-directory root. Runtime files are isolated
under `tenants/<tenant_id>/devices/<device_id>/`; there is no fallback to an
older unscoped database. Paths below are relative to the state directory:

| Path | Contents |
| --- | --- |
| `config.toml` | Public collector, namespace and operational settings. |
| `tenants/TENANT/devices/DEVICE/state.db` | Metadata outbox, tracking, counters, exporter health, quarantine and encrypted raw objects. |
| `tenants/TENANT/devices/DEVICE/export.log`, `hook.log` | Diagnostics; files over 5 MiB rotate at process startup. |
| `tenants/TENANT/devices/DEVICE/export.lock` | Exporter singleton lock. |

The old plaintext full-payload debug JSONL capture has been removed.
Ciphertext remains after capture or sync is disabled, with no automatic deletion.
On Unix, managed directories use `0700`, and files and SQLite sidecars use
`0600`. Permissions supplement encryption and do not protect against a process
already authorized to unlock a reader key.

Gently accepts only its encrypted local schema. Supported encrypted state opens
without a reset; an incompatible database, such as plaintext state from early
development builds, is rejected. Stop hooks and exporters, then explicitly
remove or relocate disposable application state and its SQLite sidecars before
initializing fresh state. Review old debug files and backups separately. Gently
does not migrate plaintext rows or accept old digest-based raw references.
Never reset the separate credential vault as part of an application-state reset.
