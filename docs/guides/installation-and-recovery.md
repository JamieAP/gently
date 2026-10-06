# Installation, upgrade and recovery

Current builds are pre-v1 source installs. No v1 release artifacts or tags are
promised by this guide. macOS and Linux are the target platforms; Unix file/socket
permissions underpin local privacy. A release must name its tested OS and CPU
architecture. Windows support has no v1 acceptance evidence.

Source installation needs Git and stable Rust/Cargo. Running the local Worker
also needs Node.js 22/npm and Python 3; encrypted capture needs public age reader
policy, while decryption needs an enrolled reader and its supported age provider.
Hardware readers require the platform plugin. Rust tests alone do not verify it.

Pin a reviewed full commit (or a published release tag when available), then run:

```sh
cargo install --locked --path crates/gently-cli
# Install only the coding agents you use:
gently init --codex
gently init --claude
gently config --check
gently status
```

Stop the exporter before replacing its binary, then refresh hooks/MCP with `init`
and restart coding agents so they load the new executable. `init` preserves user
preferences and existing Gently configuration. Use a stable executable path.

## Private consistent state backup

```sh
gently state backup /private-backups/gently-state.db
```

The parent directory must already exist. SQLite's backup API includes live WAL
changes; copying just state.db is unsafe. Backup publication is exclusive and
never overwrites an existing path. The file is owner-only and pins the configured
tenant/device. It contains private metadata and opaque encrypted raw objects;
it is not encrypted as a whole and must never be committed or uploaded publicly.
Capture and backup need no reader key. No credentials, policy files or reader
identities are copied by this command.

Keep signed public policy, trust pins and an independent recovery reader through
your existing secure recovery process. A database backup cannot replace a lost
reader or owner signing identity. Test recovery with synthetic state before
relying on it; do not decrypt keys into files or share them in public reports.

## Restore and schema upgrades

Stop Gently and the coding agents. Preserve the old namespace privately and
configure the same tenant/device in a fresh state directory. `state.db` must be
absent, including WAL/SHM/journal sidecars; restore refuses every existing file, so newly captured state is never
replaced. Then run:

```sh
gently state restore /private-backups/gently-state.db
gently config --check
gently status
```

Restore accepts only Gently backup format 1 containing supported encrypted local
schema 2, with matching tenant/device. Plaintext legacy state and unsupported
schemas are rejected. Raw ciphertext, outbox, quarantine and lifecycle state are
restored together; install reader/policy recovery separately, verify decryption,
then restart the exporter. Existing supported encrypted state opens without a
reset. Future schema changes must include a reviewed migration and backup/restore
coverage or explicit version refusal; never silently import old plaintext traces.

## Uninstall

Run `gently uninstall --codex` or `gently uninstall --claude` using the installed
executable before removing that binary. Uninstall matches exact managed commands
and MCP arguments for that executable, including the legacy quoting form. It
preserves custom wrappers, unrelated hooks/preferences, local state, policy and
keys. Registrations for another executable path remain for manual review. Restart
coding agents and stop local collector/export processes separately. It does not
remove the credential vault or delete captured history.

## Versioned release contract

Before publishing binaries, the tag must match the workspace package version and
the exact reviewed source commit. Each tested OS/architecture needs checksummed
artifacts, build provenance, a verified install/upgrade path and recovery checks.
Builds must avoid embedding personal source paths. Artifact publication and a v1
tag remain pending until the final-tree Mac/Linux and native four-surface gates
pass. The separate release-verification PR tracks those gates.
