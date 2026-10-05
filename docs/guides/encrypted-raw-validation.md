# Encrypted raw validation

The automated acceptance test uses invented data, disposable software keys,
an isolated HOME and actual local Wrangler/workerd/D1. It never opens the
credential vault, an existing hardware identity or a cloud account. Run it
from a clean checkout after installing dependencies:

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
cd worker
npm ci
npm test
npm run typecheck
cd ..
python3 -m unittest discover -s scripts -p 'test_*.py'
python3 scripts/acceptance-local.py
```

To exercise an installed binary, pass `--binary /absolute/path/to/gently`.
The code CI runs these checks and installs the CLI from source on macOS and
Linux. A locally passing macOS run does not establish a passing Linux CI run;
inspect both jobs on the PR. Failures deliberately withhold child output so
fixture passphrases, bearer tokens and raw content do not enter CI logs.

| Area | Evidence required |
| --- | --- |
| Setup and configuration | Fresh and repeated Claude/Codex installs; preserve comments, unrelated hooks and MCP preferences; inline TOML tables; malformed types leave files intact; file/environment precedence; separate capture/sync/resolve flags; bounded numbers; absolute raw paths; private filesystem/link checks; incompatible schemas reject without deleting state. |
| Private readers | Immediate input after a prompt sees echo already disabled; terminal flags restore after success, mismatch, empty input, cancellation and wrong unlock; missing readers do not block metadata or MCP initialization; raw access without a terminal fails; request decryption is deduplicated and every row binding remains enforced. |
| Crypto and enrollment | Wrong identities fail, tampering fails, owner signatures and pinned manifests reject expiry/rollback/same-epoch forks; a second reader unlocks the owner backup; enrollment changes deny new ciphertext to removed readers while historical access remains. |
| Local persistence | Capture and lifecycle/outbox changes are atomic; ciphertext capacity cannot be overspent by concurrent admission; metadata continues when capture fails; ciphertext cache capacity does not prevent in-memory verified reading; no plaintext appears in SQLite, WAL or logs. |
| Real collector | Both hook formats encrypt and export; restart/outage/auth correction preserve queues; actual D1 accepts separate tenants with shared span IDs; authenticated host ownership and capability checks hold; raw refs cannot cross tenants; more than 65 queued envelopes exercise actual 32-span admission and exporter bisection. |
| Export recovery | Raw rejection quarantines ciphertext without blocking metadata; explicit retry preserves exact objects; authentication failure remains fatal; raw-only success clears old health failures. |
| Process lifetime | Ctrl-C and service crashes stop both owned process groups, including stubborn descendants and children of an exited parent; the actual Worker listener closes; disposable state is removed. |

The real acceptance script covers the complete software-reader route through
the CLI and MCP stdio, protected owner recovery, new enrollment epochs,
quarantine and retry, tenant isolation, batches, and byte scans of actual
persisted state. Rust, Worker and launcher regressions exercise the remaining
fault cases and boundary conditions without changing live user setup.

## Mac hardware reader acceptance

With the trusted plugin installed, run the same actual local acceptance with a
new disposable Secure Enclave primary reader and protected software recovery:

```sh
python3 scripts/acceptance-local.py --hardware
```

Be available for the native authorization prompts. The runner never answers
them, reads the credential vault, or uses an existing hardware identity. It
checks hardware CLI/MCP decryption and owner signing, then removes its own
temporary identity files and local collector state.

Run this separately with a new disposable Secure Enclave reader and an
independent recovery reader. Hardware prompts require the operator's normal
authorization. Do not use the credential-vault identity for this exercise.

1. Enroll the public native tag recipient, sign and independently verify its
   trust policy, then capture invented content with no reader private key on
   the capture host. Confirm capture/export never prompts for hardware access.
2. Query that content from a foreground reader process and through the
   intended MCP launch path. Confirm hardware authorization, decrypt success,
   one decrypt operation per repeated object within a request, and no prompt
   on metadata-only operations.
3. Cancel or deny authorization, remove the external reader executable, and
   exercise the bounded timeout path. Confirm fixed errors, no plaintext
   output, no hanging child process and intact ciphertext/metadata state.
4. Restore the reader and recover owner signing through the second enrolled
   device. Advance enrollment, remove the hardware reader and its collector
   credential, and confirm denial for newly encrypted data. Historical copies
   and keys already obtained remain decryptable.

The software acceptance test and an external-process adapter test cannot
establish Touch ID behavior or GUI MCP foreground authorization. Record the
hardware, plugin/age versions, launch method and outcomes before treating that
reader setup as validated.

## Staging Cloudflare acceptance

The staging runner uses the existing Wrangler login and creates a fresh Worker
and D1 database for its run. Review the public plan, then execute the same nonce:

```sh
python3 scripts/acceptance-staging.py --plan \
  --account ACCOUNT_ID --run-id FRESH_16_HEX \
  --manifest-out /private/path/public-validation.json
python3 scripts/acceptance-staging.py --execute \
  --account ACCOUNT_ID --run-id FRESH_16_HEX \
  --manifest-out /private/path/public-validation.json
```

`ACCOUNT_ID` is the selected public account identifier. Generate a nonce with
`python3 -c 'import secrets; print(secrets.token_hex(8))'`. The runner refuses
existing resource names, sends random host credentials to Wrangler through
stdin, checks the deployed API and remote D1, then deletes only its own
resources. The public manifest checkpoints resource IDs and cleanup outcomes
without credentials or keys. SIGTERM/keyboard interruption runs cleanup;
after a forced kill or machine loss, use those recorded IDs to investigate and
remove any remaining test resources. `--secure-enclave` adds a fresh Mac
hardware reader and requires normal operator authorization.

Use a deliberately selected staging Worker and fresh D1 database. Configure
per-host `GENTLY_HOSTS` authorization through the deployment secret mechanism;
each token grants only its tenant/device and required ingest/read capability.
Keep private reader and owner keys on enrolled devices. Cloudflare deployment
secrets protect service authorization, not device decryption keys.

Repeat the actual acceptance scenarios against staging with two tenants and
multiple Mac/Linux hosts. Confirm opaque raw storage, tenant and host isolation,
capability failures, immutable conflicts, revocation of bearer credentials,
bounded requests and the selected plan's D1 query/storage limits. Validate
cloud retention, backups, monitoring and rotation against the deployment
policy, and exercise the configured transport (including HTTP/3 when used).

Inspect deployed bindings, logs, metrics and backups for invented canaries;
test workload and quotas on the selected plan. Record results separately from
local D1 emulation. Running the staging script provisions only its disposable
validation resources. It does not automate production deployment or cloud key
enrollment, and it does not prove writer origin/full replay resistance against
a malicious cloud.
