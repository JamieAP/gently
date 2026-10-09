# Security and privacy

Gently encrypts opt-in raw capture before persistence, using only the public
keys of enrolled readers. Cloudflare stores opaque ciphertext; reader private
keys stay on devices. Ordinary traces still expose identifying metadata and
activity patterns, so choose where that metadata may be sent.

## What is stored where

| Location | Contents |
| --- | --- |
| Collector metadata | IDs, names, timings, statuses, paths, host/session context, byte lengths and optional opaque raw references. |
| Collector raw table | Immutable tenant-scoped age ciphertext envelopes when raw sync is enabled. |
| Local `state.db` and SQLite sidecars | Metadata outbox, tracking, counters, quarantine and optional encrypted raw objects. |
| Local logs | Operational diagnostics; processing failures use fixed messages without raw payloads. |

Public content hashes are not emitted, even when capture is disabled. Random
raw references do not reveal whether two captured values are equal. Byte
lengths, ciphertext sizes, paths, hostnames, session IDs and timing patterns
remain visible. Encryption does not make that metadata anonymous.

## Three separate opt-ins

| Control | Process | Effect |
| --- | --- | --- |
| `capture_raw_values` / `GENTLY_CAPTURE_RAW_VALUES` | Hook | Encrypt selected content and complete hook JSON to approved public recipients, then retain ciphertext locally. |
| `sync_raw_values` / `GENTLY_SYNC_RAW_VALUES` | Exporter | Upload retained ciphertext without a reader identity. |
| `resolve_raw_values` / `GENTLY_RESOLVE_RAW_VALUES` | CLI/MCP reader | Explicitly unlock a reader identity and hydrate referenced fields in memory. |

All three default to false. Capture uses an owner-signed manifest and a local
trust pin; it does not open a private key, invoke a plugin or request biometric
unlock. Missing, invalid, expired or rolled-back policy never falls back to
plaintext. See [enrollment](../guides/encrypted-raw-values.md) for the exact
policy and setup workflow.

Reader resolution authenticates the encrypted context and field-to-span
binding before adding content to a result. It uses retained local ciphertext,
or authenticated cloud ciphertext when needed. Wrong keys, corrupted objects,
changed tenant/context and mismatched field bindings are rejected. Decrypted
output can include credentials, commands or source material, and MCP output
can enter the calling agent's model-provider context.

`gently init --claude --resolve-raw-values` and the equivalent Codex command
set the MCP registration opt-in. Reinstalling without the flag removes that
registration setting; independently configured settings still apply.

Capture retains selected field maps. The previous plaintext full-payload debug
JSONL path has been removed, and no process-environment snapshot is captured.

## Enrollment, recovery and revocation

Each tenant pins its own owner verification key. That owner signs public
reader enrollment, key epochs and expiry. Install the root through an
independently verified trusted-device channel; do not trust a recipient key
simply because the cloud returned it. Capture checks the signed policy against
the local root, minimum epoch and exact manifest digest at that epoch. A
different same-epoch policy is rejected. Advancing enrollment updates the trust
pin on each capture host, and expired leases stop raw retention.

Authentication credentials and raw-decryption identities are different keys.
A capture-only Linux host needs public policy and an ingest credential, not a
reader identity. Software reader identities are passphrase encrypted; they do
not provide Secure Enclave hardware protection. Macs may use a dedicated
Secure Enclave identity and native age tag recipient. Gently does not reuse or
modify the credential vault's identity. Software reader queries and signing
require an attached private terminal for passphrase entry. A GUI MCP process
without a terminal needs an interactively launched reader process or an enrolled
hardware reader; passphrases are never read from the environment.

Enroll a recovery reader and test recovery before keeping long-lived data.
An additional recipient can recover objects encrypted to it; the signing
owner key needs its own protected recovery plan. New enrollment applies to
future objects. There is no automatic historical re-encryption command.

Revoke cloud access by removing a host's credential record, then remove its
reader key, sign a higher-epoch manifest and update every capture host's policy
and trust pin. Revocation cannot erase ciphertext or plaintext already copied,
and an offline writer can use its last valid policy until its lease expires.

Age authenticates ciphertext and encrypted context; public-key encryption
does not prove which capture host authored an object. This version has no
writer signatures or full replay/provenance protocol. A malicious cloud can
fabricate an entirely new encrypted object using public recipients, replay old
objects, alter visible metadata or deny service. Context/binding checks detect
ordinary substitutions, but do not establish an enrolled writer's origin.

## Collector authorization and Cloudflare tooling

Clients receive `GENTLY_TOKEN` only through their environment. The Worker stores
an operator-managed `GENTLY_HOSTS` secret mapping each credential to one tenant,
one device and ingest/read capabilities. Every route requires a named tenant,
and every database operation is scoped to the authenticated tenant. Ingest-only
hosts cannot query metadata or raw ciphertext. Local defaults name one personal
tenant; additional tenants have separate authorization and encryption roots.

The current bearer map has no automatic credential expiry. Operators rotate
or revoke individual entries. HTTPS protects remote transport; local HTTP is
bound to loopback. Keep credentials out of source, argument lists and logs.
Raw endpoints limit JSON requests to 1 MiB and decoded ciphertext to 512 KiB.
The Worker checks public age framing, not cryptographic payload integrity;
client encryption and enrolled reader verification provide that boundary.
Known raw aliases and `.sha256` attributes are rejected at metadata ingest,
but arbitrary externally supplied text cannot be classified as private by
these guards.

[Cloudflare Access service tokens](https://developers.cloudflare.com/cloudflare-one/access-controls/service-credentials/service-tokens/)
can add per-host admission, expiry and rotation in a future deployment. This
repository does not provision Access or send its service-token headers.
[Secrets Store](https://developers.cloudflare.com/secrets-store/manage-secrets/)
manages credentials for Cloudflare services; its management API does not
return secret values for a Mac/Linux host vault. It must not hold raw reader
keys or act as a plaintext decryption broker. D1 stores bounded ciphertext;
R2 is deferred until larger objects justify it.

## Local files and development state

Unix managed directories use `0700`; files and SQLite sidecars use `0600`.
Final-path symlinks, foreign-owned files and multiple hardlinks are rejected
before changing private files. Permissions do not protect unlocked plaintext
from its reader process, the same user or privileged code. Backups and copies
have their own access and recovery policies.

Runtime state, logs and locks are isolated under
`<state_dir>/tenants/<tenant_id>/devices/<device_id>/`; public config remains at
the state-directory root. No unscoped state fallback is read.

Each tenant/device state database admits at most 64 MiB of encoded ciphertext
envelopes; SQLite pages and WAL overhead are additional. At capacity, capture
skips new raw data while retaining metadata, and a reader can decrypt a fetched
object in memory without adding it to the cache. No existing or pending object
is evicted automatically. Cloud quotas and deletion schedules are an explicit
future deployment policy.

There is no automatic ciphertext or collector retention policy. Turning off
capture or sync stops new work; it does not erase existing data. Gently accepts
only its encrypted schema. To discard plaintext state from early development
builds, stop Gently before explicitly resetting that disposable state, SQLite sidecars and debug files; no legacy
reader, migration or alias is retained. Deletion cannot prove erasure from SSD
snapshots or backups. Preserve the separately installed credential vault.

## Launchers and process credentials

The local scripts perform dependency preflight and require an inherited token.
An optional foreground secret-provider wrapper handles private unlock. The
supervisor constructs the Worker authorization binding in memory and starts
the exporter; those processes necessarily hold their runtime credentials.
No hook initiates hardware unlock.

An export watcher does not authorize a separate CLI or MCP process. Each needs
its own inherited read credential and, for raw output, an enrolled reader
identity. See [local setup](../getting-started/local-collector.md) and
[architecture](architecture.md).

## Local desktop query delegation

On Unix, an unlocked `gently export --watch --serve-queries` can serve tokenless
CLI/MCP processes through a 0600 socket in the private tenant/device runtime
directory. Requests must match the watcher's collector URL and tenant. Socket ownership
and both peers must match the current effective user ID; mode bits alone do not
exclude extended ACL access. The
watcher permits only bounded metadata queries and encrypted-object downloads;
it never returns its token or decrypts content. Reader resolution still requires
an explicit enrolled reader identity and verifies all ciphertext bindings.
Processes running as the same local user can access this read-only delegation.
