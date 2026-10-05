# Encrypted raw values

Enroll reader devices before enabling raw capture. Capture hosts use only a
signed public recipient policy; private reader identities are used by explicit
reader/owner commands. This works across Macs and Linux hosts without putting
decryption keys in Cloudflare.

Capture, cloud sync and resolution are three independent opt-ins. Ordinary
metadata capture requires none of them. Use a fresh development database:
Gently rejects earlier development schemas and offers no compatibility reader
or migration. Preserve any data you need before explicitly choosing a fresh
state directory. Commands never delete an incompatible database for you.

## 1. Create reader identities

For a software reader on a Mac or Linux host:

```sh
install -d -m 700 ~/.gently/keys ~/.gently/policy
gently raw identity --out ~/.gently/keys/reader.age
```

Choose a passphrase in the private interactive terminal prompt. The command
writes an encrypted identity and prints only its public age recipient. Keep
that public recipient for the manifest. No passphrase or private key belongs
in a command argument, environment file or conversation.

Repeat on an independently enrolled recovery device. Software keys are
passphrase protected, not hardware bound. A capture-only host needs neither
identity nor passphrase.

### Optional Mac Secure Enclave reader

With a separately installed, trusted `age-plugin-se`, create a new identity
for raw data, distinct from any credential-vault identity:

```sh
age-plugin-se keygen --access-control any-biometry-or-passcode --recipient-type tag -o ~/.gently/keys/reader-se.txt
age-plugin-se recipients -i ~/.gently/keys/reader-se.txt --recipient-type tag
```

Use the public `age1tag1...` recipient in enrollment and `reader-se.txt` as the
reader identity. Native tag encryption avoids a capture-side plugin. Decryption
requires the plugin and its normal foreground hardware authorization. The
[plugin's documentation](https://github.com/remko/age-plugin-se) describes the
recipient formats and non-transferable keys. Gently's reader integration uses
a trusted age/rage executable path; test this reader and recovery device before
relying on it. No existing credential identity is moved or changed.

## 2. Create the tenant owner key

On a trusted owner device, use public reader recipients to protect the signing
key, including the recovery reader:

```sh
gently raw owner-key \
  --recipient AGE_PUBLIC_READER_RECIPIENT \
  --recipient AGE_PUBLIC_RECOVERY_RECIPIENT \
  --out ~/.gently/keys/owner.age
```

The placeholders are public recipient strings. This writes only an encrypted
owner key and prints the public owner verification key in base64. Verify that
public key through a trusted channel when enrolling another host. Keep a
protected backup of the encrypted owner key and test that a recovery reader
can unlock it; adding a recovery recipient to raw manifests alone does not
recover a signing key encrypted only to a lost device.

## 3. Sign a public reader manifest

Prepare a public `unsigned.json` with this shape. Replace the recipient and
expiry placeholders; `expires_unix_secs` must be an integer UTC timestamp:

```text
{
  "version": 1,
  "tenant_id": "personal",
  "key_epoch": 1,
  "expires_unix_secs": FUTURE_UNIX_SECONDS,
  "readers": [
    {"device_id":"mac-reader","key_id":"mac-reader-1","recipient":"AGE_PUBLIC_READER_RECIPIENT"},
    {"device_id":"linux-recovery","key_id":"linux-recovery-1","recipient":"AGE_PUBLIC_RECOVERY_RECIPIENT"}
  ]
}
```

Choose a short validity lease and a refresh schedule you can maintain. For
example, calculate a public timestamp seven days from now:

```sh
python3 -c 'import time; print(int(time.time()) + 7 * 24 * 60 * 60)'
```

Tenant/device/key IDs use 1–64 ASCII letters, digits, dashes or underscores.
Reader device IDs, key IDs and public recipients must be unique; enroll 1–64
readers. Each tenant uses its own owner key and reader set. Epochs are positive
integers; increase the epoch when changing enrollment.

Sign the manifest by explicitly unlocking an enrolled identity that can decrypt
the owner key:

```sh
gently raw sign \
  --manifest unsigned.json \
  --owner-key ~/.gently/keys/owner.age \
  --identity ~/.gently/keys/reader.age \
  --out ~/.gently/policy/manifest.json
```

The signed public file contains `{ "manifest": ..., "signature_b64": ... }`.

## 4. Install the owner trust pin

Verify the public owner key independently, then install it:

```sh
gently raw trust \
  --manifest ~/.gently/policy/manifest.json \
  --owner-public OWNER_PUBLIC_KEY_BASE64 \
  --out ~/.gently/policy/trust.json
```

The trust file contains `tenant_id`, `owner_verify_key_b64`, `min_epoch` and
`manifest_digest`, which pins the exact public manifest at the remembered epoch.
The command verifies the signed policy and refuses to replace an existing
owner root, lower its minimum epoch or accept a different manifest at that same
epoch. It does not silently enroll a cloud-provided key.

Copy the signed manifest and independently verified trust pin to each approved
capture host through a trusted channel. Those files are public policy; private
reader and owner keys stay on their enrolled devices. Refresh the signed policy
and trust pin on every writer when advancing an epoch. No untrusted cloud
recipient-discovery endpoint is used.

## 5. Enable capture, sync and reading separately

On a capture host, configure absolute paths:

```toml
tenant_id = "personal"
device_id = "mac-main"
capture_raw_values = true
raw_manifest = "/absolute/path/manifest.json"
raw_trust = "/absolute/path/trust.json"
```

Capture encrypts selected field maps once per event with random opaque
references. The encrypted payload binds tenant, device, epoch, event, session,
harness and field-to-span ownership. A missing, invalid, expired or rolled-back
manifest skips raw retention while metadata capture continues; it never writes
plaintext or public content fingerprints. Capture retains the selected field
map; the previous plaintext full-payload debug capture has been removed.

To upload ciphertext, enable `sync_raw_values = true` for the exporter and
supply its ingest credential. The credential's enrolled device must match the
capture device. The exporter needs no reader identity. When sync is enabled,
it attempts pending ciphertext before draining metadata. A rejected object
(HTTP 400, 409, 413 or 422) stays encrypted in quarantine while metadata and
other raw objects continue exporting. Authentication failures preserve the
active queues and stop export. A reference does not guarantee immediate
availability. Inspect `gently status` for raw quarantine counts, bytes and the
last rejection status. After investigating and correcting the collector,
`gently export --retry-raw-quarantine` requeues retained objects without editing
their ciphertext. A persistent immutable-object conflict will be quarantined
again; retry does not overwrite an existing cloud object.

On an enrolled reader, configure:

```toml
resolve_raw_values = true
raw_identity = "/absolute/path/reader.age"
```

Provide a read credential for that tenant, then query normally. Resolution
uses local ciphertext first and can fetch missing objects from the collector.
It unlocks the reader only when a query needs referenced raw ciphertext,
verifies context and span bindings, and adds raw attributes to the in-memory
result. Metadata trace lists, statistics and MCP initialization work without
unlocking a reader. A request decrypts each object once and checks its bindings
for every result row; decrypted values are never cached on disk. For MCP,
register the opt-in with `gently init --claude --resolve-raw-values` or the
corresponding Codex command. Decrypted MCP output may be sent to the calling
agent's model provider. Software identities require an attached private terminal
for passphrase entry during reading and signing. A GUI-launched MCP process
without a terminal cannot unlock that identity: launch its reader process from
an interactive terminal or use an enrolled Mac hardware reader. Never supply
passphrases through environment variables or command arguments.

Objects support up to 256 KiB of serialized plaintext and 512 KiB of age
ciphertext. The collector bounds each JSON request to 1 MiB. Oversized raw
content is not retained; metadata byte lengths remain visible. Each tenant/device
state database admits up to 64 MiB of encoded ciphertext envelopes, excluding
SQLite page and WAL overhead. At capacity, new raw capture is skipped while
metadata continues; readers can still decrypt fetched objects in memory without
caching them. Existing objects, including pending uploads, are never evicted
automatically. Inspect `gently status` for retained and pending object byte
counts, including retained quarantine. Cloud quotas and retention schedules
require a separate deployment policy.
Reader resolution also bounds aggregate encoded cached ciphertext and expanded
attribute/resource JSON to 8 MiB per request; narrow a query that exceeds it.
This is a data-admission bound, rather than a measurement of total process memory.

`gently config --json` exposes only resolved public setup fields. Local
launchers use the same resolution as hooks and queries, including file
tenant/device settings and environment overrides. Raw policy and identity
paths must be absolute; `~` inside TOML is not expanded. Numeric settings are
bounded: `export_batch` 1–4096, `outbox_cap` 1–1000000, and export/query timeouts
1–3600 seconds. Invalid settings fail before export changes queues or health.
Run `gently config --check` or the local launcher's `--check` before starting:
these check public capture policy, configured reader path and an existing
state schema without requesting credentials or unlocking an identity.

## Enrollment changes and revocation

Before keeping long-lived data, verify wrong-device denial and recovery with
invented content. New readers can decrypt future objects after enrollment;
old objects remain encrypted to their original readers. No automatic
historical re-encryption or recovery transfer is provided.

To revoke a device, remove its collector credential, remove its public reader
key, sign a higher epoch and refresh policy/trust on every capture host. An
offline writer can still use its last valid policy until expiry. Copies already
obtained with an enrolled key cannot be revoked.

Recipient signatures authorize encryption policy. Raw objects do not carry
writer signatures, so they do not prove origin or provide a full replay
protocol against a malicious cloud. See
[security and privacy](../concepts/security-and-privacy.md) for confidentiality,
metadata exposure, retention and this provenance limit.

See [validation and acceptance](encrypted-raw-validation.md) for the automated
checks and the hardware and cloud checks needed before relying on this setup.
