# Architecture

Gently separates metadata capture, encrypted raw retention, delivery and
reading. Hooks persist metadata and optional ciphertext locally; exporters
send them to tenant-scoped collector endpoints. Reader keys remain on enrolled
devices.

```text
Claude Code / Codex hooks
           |
       gently hook <--- signed public reader policy + local owner pin
           |
   local SQLite state.db <--- gently status
     | metadata  | optional ciphertext
     +-----------+
           |
   gently export [--watch]  (ingest credential, no reader key)
           |
      Worker -> tenant-scoped D1
           ^
           | read credential: metadata + opaque ciphertext
    CLI / MCP ---> enrolled reader identity ---> in-memory raw output
```

## Components

| Component | Responsibility |
| --- | --- |
| `gently hook` | Parse events, update span bookkeeping, encrypt approved raw fields and queue metadata. |
| `gently-raw` | Age encryption/decryption, signed recipient policy, tenant context and field bindings. |
| Local `state.db` | Outbox, tracking, counters, quarantine and immutable encrypted raw objects. |
| `gently export` | Drain metadata; optionally sync ciphertext without decrypting. One exporter holds the state lock. |
| Worker and D1 | Authenticate a host, authorize its tenant/capabilities and store metadata plus bounded ciphertext. |
| CLI and MCP | Query metadata; optionally unlock an enrolled identity and resolve referenced content locally. |

The default local database is
`~/.gently/tenants/personal/devices/local/state.db`; config remains at
`~/.gently/config.toml`. Runtime files are scoped by tenant/device. Collector D1
is distinct:
queued metadata does not appear in collector queries before delivery. Local
raw caching does not make SQLite a second trace-history query server.

## From hook to storage

1. The harness starts `gently hook` with one JSON event on stdin.
2. The adapter extracts metadata and byte lengths. Public content hashes are
   removed regardless of raw-capture settings.
3. With capture enabled, the hook verifies a locally installed signed manifest
   against its tenant owner pin, minimum epoch, pinned manifest digest and expiry. Only public
   recipients are needed; no reader identity or hardware prompt is used.
4. Approved raw fields receive random `.raw_ref` pointers. Age encrypts the
   event context, fields and owning span IDs before any raw persistence.
5. Lifecycle state, ciphertext and the OTLP envelope commit together. Invalid
   raw policy skips raw retention while preserving metadata capture.
6. A token-bearing hook may start a detached exporter; a tokenless hook leaves
   work queued for a watcher or later drain.

The hook emits no normal stdout and contains processing errors and panics.
A successful exit is not proof of capture or delivery. See
[reliability](reliability.md) and local exporter health.

## Delivery and reading

Metadata is OTLP/JSON, with tenant selection on each request. Optional raw sync
uses the ciphertext endpoint. Objects are immutable: identical retries succeed,
conflicting content for the same tenant/ref is rejected. With sync enabled,
pending ciphertext is attempted before metadata, and an upload failure can stop
the drain. References do not guarantee immediate availability.
Remote metadata delivery can prefer HTTP/3 with TCP fallback.

Every client inherits an environment-only bearer token. The Worker maps it to
one tenant/device and ingest/read capabilities. All D1 keys, upserts and queries
include that tenant. Additional tenants use separate owner roots and reader
sets; there is no global raw-decryption key.

Resolution checks the ciphertext context and field-to-span bindings, decrypts
with an enrolled reader and hydrates only the in-memory query result. Cloudflare
has no reader key. Recipient policy is signed, but raw objects have no writer
signature or complete replay/provenance protocol. These are distinct trust
boundaries; see [security and privacy](security-and-privacy.md).

## Launch boundary and state

Provider-neutral local launchers require an inherited `GENTLY_TOKEN`, offer
credential-free dependency preflight and supervise the Worker/exporter. An
optional foreground Mac secret helper wraps those launchers externally. The
supervisor builds `GENTLY_HOSTS` for the Worker in memory; the exporter receives
the client credential. A watcher does not authenticate other CLI/MCP processes.

Gently 1.0 accepts only the encrypted local schema. Supported encrypted state
opens without a reset; future schema changes need reviewed migrations or an
explicit version refusal. Plaintext state from early development builds is not
migrated (no digest aliases or dual readers) and requires an explicit reset
while Gently is stopped. The external credential
vault is preserved. See [local setup](../getting-started/local-collector.md),
[raw enrollment](../guides/encrypted-raw-values.md),
[trace model](trace-model.md) and [collector reference](../reference/worker.md).
