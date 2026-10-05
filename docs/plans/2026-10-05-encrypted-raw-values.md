# Encrypted raw values implementation plan

> **For Codex:** Execute this plan in the current isolated worktree using the executing-plans, test-driven-development, verification-before-completion and requesting-code-review skills. The user has authorized execution and preparation of a review PR.

**Goal:** Remove Gently's dependence on a macOS secret helper and provide encrypted local/cloud raw capture that enrolled Macs and Linux readers decrypt, with tenant isolation from the first schema.

**Architecture:** Launchers consume an inherited credential; an external secret provider owns credential acquisition. Hooks use a locally pinned, owner-signed, expiring recipient manifest to encrypt contextual field maps with age before persistence. A ciphertext-only local queue and tenant-authorized Worker transport objects without decryption keys; explicitly enabled CLI/MCP readers decrypt locally and verify field/span bindings.

**Tech stack:** Rust, age recipient encryption (native X25519/tag; explicit reader plugin identities), Ed25519 manifest signatures, SQLite, TypeScript Cloudflare Workers/D1, Vitest and isolated Python launcher fixtures.

This is a clean pre-public replacement. There are no compatibility launchers, legacy raw hashes/readers, plaintext raw storage, migration/backfill paths or automatic state deletion. The installed secret vault, protected identities and agent OAuth state are outside the change. D1 holds bounded ciphertext objects initially; R2 and Access provisioning are documented deployment options, not new infrastructure in this PR.

## Shared contracts

- `GENTLY_TOKEN` is inherited by clients. Worker `GENTLY_HOSTS` is a JSON secret containing `{token, tenant_id, device_id, capabilities:["ingest","read"]}` principals, with no old shared-token fallback. All authenticated routes require the matching `tenant_id` query parameter.
- `Manifest {version:1,tenant_id,key_epoch,expires_unix_secs,readers:[{device_id,key_id,recipient}]}` is signed by the pinned owner. Capture accepts only native public recipients; no reader key or hardware prompt is needed. `TrustPin {tenant_id,owner_verify_key_b64,min_epoch,manifest_digest}` rejects changed tenants, tampered manifests, stale epochs, same-epoch policy forks and expired policies.
- `RawObject {version:1,context:{tenant_id,device_id,key_epoch,raw_ref,session_id,harness,event},ciphertext_b64}` contains a random 32-hex reference. Its encrypted payload repeats the context and includes selected field values and field-to-span bindings. Limits: 256 KiB plaintext, 512 KiB ciphertext, 1 MiB HTTP body. Errors never include payloads or keys.
- OTLP attributes reference `<field>.raw_ref`. Public content fingerprints are removed. Metadata remains visible; encryption does not claim to authenticate a writer against a malicious cloud or prevent replay of an entire valid trace. Reader validation binds decrypted fields to expected tenant/session/harness/span/reference.
- Capture, raw synchronization and raw resolution are separate false-by-default controls. Encryption/configuration failure omits raw retention with safe diagnostics while hook metadata capture remains silent and exits zero.

## Task 1: Crypto and enrollment format

**Files:** create `crates/gently-raw/{Cargo.toml,src/lib.rs}` and focused crypto tests; update workspace lockfile.

1. Write tests for tampered/expired/wrong-tenant/rollback recipient manifests, native public-only capture, two-reader decryption, wrong identity, context/ref/span swaps, size bounds and unknown fields.
2. Run `cargo test -p gently-raw` and record expected failures before implementation.
3. Implement `VerifiedManifest::verify`, `seal`, `open`, random refs, strict contextual objects and bounded standard age encryption. Add zeroizing owner-key helpers and protected software reader identity generation/loading for explicit reader commands.
4. Run the focused tests. Pin dependency versions; no custom AEAD/key-wrap protocol.

## Task 2: Encrypted storage and hook capture

**Files:** replace `crates/gently-store/src/raw_values.rs`; update store schema/transaction API, `crates/gently-cli/src/{local_raw,cmd_hook}.rs`, harness field handling and `crates/gently-cli/tests/hook.rs`.

1. Write failing tests for plaintext canaries absent from DB/WAL/log/outbox, no token/private key capture, no unsafe fallback, atomic raw/span/outbox rollback, scoped immutable objects and repeated-value distinct refs.
2. Run `cargo test -p gently-store` and relevant hook tests; record failure causes.
3. Replace plaintext table with tenant-scoped ciphertext objects and synchronization status and a 64 MiB admission budget that preserves existing objects. Reject an old plaintext schema with an explicit development-state-reset error; do not reset it.
4. Prepare refs before span application; encrypt field maps with bindings to emitted/open spans; persist object, spans and outbox atomically. Remove plaintext debug files and public content hashes.
5. Run focused tests and inspect actual fixture DB/WAL bytes for the canary.

## Task 3: Tenant-authorized cloud ciphertext API

**Files:** `worker/{schema.sql,vitest.config.ts,wrangler*.toml,src/*,test/*}`.

1. Add failing tests for cross-tenant trace/object access, ingest-only read denial, invalid/ambiguous credential config, immutable replay/conflict, malformed/oversize ciphertext and plaintext raw attribute rejection.
2. Run `npm test` in `worker`; verify expected failures.
3. Implement per-device principal lookup, query tenant matching, composite tenant/span keys and tenant predicates in every query/upsert/CTE.
4. Add POST `/v1/raw-values` and GET `/v1/raw-values/:raw_ref`, strict envelope/age-header validation, bounded streaming request reads, a 32-span admission limit for Free D1 query budgets, and immutable idempotence.
5. Run Worker tests and TypeScript checks. Never provision or deploy Cloudflare resources.

## Task 4: CLI configuration and protected enrollment commands

**Files:** `crates/gently-cli/src/{config,main,cmd_init,cmd_raw}.rs`, CLI manifest/dependency and integration tests.

1. Add failing tests for environment-only collector credentials, rejected obsolete config/env paths, separate controls and enrollment trust rollback/root-change rejection.
2. Implement tenant/device IDs and explicit paths for signed manifest, trust pin and reader identity. Capture reads public policy; reader identities are loaded only for explicit resolution.
3. Provide explicit commands to generate an encrypted software identity, generate an encrypted owner signing key, sign a manifest, and install a verified public trust policy. Prompt privately for passphrases; output only public keys and status, never private values. No command reads native authentication stores.
4. Update harness MCP installation to the new resolution option and remove raw-SHA compatibility wiring.
5. Run CLI-focused tests and help checks with synthetic temporary homes.

## Task 5: Ciphertext synchronization and local reader hydration

**Files:** `crates/gently-cli/src/{cmd_export,query_client,local_raw}.rs` and synthetic HTTP integration tests.

1. Add failing tests for a keyless exporter uploading ciphertext under its tenant, auth failure retaining the queue, immutable replay and a reader fetching/decrypting a remote object.
2. Upload only pending encrypted objects when sync is enabled; acknowledge after success. Preserve metadata export failure/retention semantics. Bound retries and response bodies; errors contain status/category only.
3. Fetch encrypted objects on a raw-resolution cache miss using tenant-scoped auth; decrypt locally and check expected field/span context. Never accept plaintext raw attributes returned by a collector.
4. Run tests covering disabled resolution, wrong device/key, context mismatch and no decrypted persistence.

## Task 6: Provider-neutral local launchers

**Files:** `scripts/{collector-local,export-local,collector-supervise.py,test_collector_local.py}`.

1. Replace helper-specific fixtures with failing tests for inherited credentials, provider-neutral missing-token handling, preflight-only execution, supervision and no secret output.
2. Make launchers require an inherited token and support shared preflight without unlocking. Construct the Worker's per-device authentication binding in memory in the supervisor; do not write `.env`/`.dev.vars` or put tokens in arguments.
3. Keep loopback, no Wrangler disk diagnostic logs, telemetry suppression and companion shutdown. Do not add old launcher shims or secret-provider selection logic.
4. Run `python3 -m unittest discover -s scripts -p 'test_*.py'`.

## Task 7: Documentation and independent review

**Files:** README, LOCAL_SETUP, CHANGELOG and affected setup/configuration/privacy/architecture/reference docs.

1. Document clean setup, state-reset requirement for development, public-only capture, protected reader enrollment, expiring policy distribution, recovery/rotation limits and visible metadata.
2. Document Cloudflare `GENTLY_HOSTS`, optional Access host admission, tenant authorization and opaque ciphertext. Secrets Store is for cloud-service credentials, not raw decryption-key distribution.
3. Review each component and the integrated diff independently. Resolve correctness/security findings before PR creation.

## Task 8: Full verification and review PR

1. Run `cargo fmt --all -- --check`, `cargo test --locked`, `cargo clippy --locked --all-targets -- -D warnings`, Worker tests/typechecking and the launcher suite.
2. Verify synthetic end-to-end capture -> ciphertext upload -> authorized fetch -> local decrypt, with no vault access or real records. Check docs links and the requirements above against the diff.
3. Commit the reviewed changes on `codex/encrypted-raw-values`, push, and create a PR with concrete behavior, validation and remaining limits. Attach the PR to this chat. Do not merge or deploy.

## Execution evidence

Focused tests observed failures before implementing encryption, encrypted capture, tenant-scoped transport, provider-neutral launchers and enrollment commands. Independent review found and resolved encrypted-plugin identity bypass, reader descendant timeout, software pinentry environment inheritance, same-epoch policy forks, incomplete raw aliases, cloud framing/context validation and device provenance enforcement.

The end-to-end fixture exercises the CLI hook, keyless exporter, authenticated tenant HTTP routes, remote cache miss, enrolled local decryption and ciphertext-only persistence in separate capture/reader state roots. Worker authorization/storage is separately exercised against the actual Workers test runtime. Hardware prompts and a real Cloudflare deployment are outside synthetic verification.

Final verification: 216 Rust tests (`cargo test --locked --quiet`), 49 Worker tests, 7 launcher tests, strict all-target Clippy, Rust formatting, TypeScript checks, whitespace checks and 117 local documentation link targets passed. No live state, vault, hardware identity or Cloudflare deployment was accessed.
