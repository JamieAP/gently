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

## Install a release build

Each release publishes one archive per tested platform, a `SHA256SUMS` file and
GitHub build provenance:

| Archive | Platform |
| --- | --- |
| `gently-X.Y.Z-aarch64-apple-darwin.tar.gz` | macOS on Apple silicon |
| `gently-X.Y.Z-x86_64-unknown-linux-gnu.tar.gz` | 64-bit x86 Linux with glibc at least as new as GitHub's `ubuntu-latest` image |

Other platforms install from source as above. Verify the checksum and provenance
before installing (this example uses the Mac archive):

```sh
gh release download vX.Y.Z --repo JamieAP/gently \
  --pattern 'gently-X.Y.Z-aarch64-apple-darwin.tar.gz' --pattern SHA256SUMS
grep ' gently-X.Y.Z-aarch64-apple-darwin.tar.gz$' SHA256SUMS | shasum -a 256 -c
gh attestation verify gently-X.Y.Z-aarch64-apple-darwin.tar.gz --repo JamieAP/gently
tar -xzf gently-X.Y.Z-aarch64-apple-darwin.tar.gz
install -m 0755 gently-X.Y.Z-aarch64-apple-darwin/gently ~/.local/bin/gently
```

Then run the `init`, `config --check` and `status` commands above with that
binary. To upgrade, stop the exporter, install the new binary at the same path,
re-run `init` for each coding agent and restart them. Every release is checked
this way from the previous version before it is published. The Mac binary is
not notarized; `gh` and `curl` downloads are not quarantined, so prefer them to a
browser.

## Private consistent state backup

```sh
gently state backup /private-backups/gently-state.db
```

The parent directory must already exist. SQLite's backup API includes live WAL
changes in one snapshot; copying just state.db is unsafe. Writers continue; the
WAL may grow until the snapshot completes. Lock contention fails for an explicit
retry. Backup publication is exclusive and never overwrites an existing path. The file is owner-only and pins the configured
tenant/device. It contains private metadata and opaque encrypted raw objects;
it is not encrypted as a whole and must never be committed or uploaded publicly.
Capture and backup need no reader key. No credentials, policy files or reader
identities are copied by this command.

Exclude `.gently-recovery-*.db*` from sync and general backup selection. A killed
process can leave an owner-only partial temporary database or journal in the
destination directory. These contain private metadata; do not use them for
restore or share their contents. Normal completion and ordinary errors clean up
the operation’s temporary files. Each backup reports interrupted temporaries it
finds beside the destination. After confirming no other backup or restore is
running, add `--remove-stale` to delete them:

```sh
gently state backup --remove-stale /private-backups/gently-state.db
```

Only files with the exact temporary name that are regular, single-link,
owner-only and owned by you are removed. Lookalikes are reported and left in
place for manual review.

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
keys. Registrations for another executable path remain for manual review. The command
reports hook/MCP removal counts and warns about retained possible registrations;
zero matches succeeds with an explicit no-removal message. Linked, multiply linked
or unowned legacy Codex hooks.json is skipped without reading its target, with a
warning; canonical TOML cleanup continues. Codex merges that file with
config.toml, so the summary counts it as an unread retained file that may still
register Gently hooks; remove them before deleting the binary. Malformed regular selected files still
abort before any edits. Restart
coding agents and stop local collector/export processes separately. It does not
remove the credential vault or delete captured history.

## Versioned release contract

Before publishing binaries, the tag must match the workspace package version and
the exact reviewed source commit. Each tested OS/architecture needs checksummed
artifacts, build provenance, a verified install/upgrade path and recovery checks.
Builds must avoid embedding personal source paths. The release workflow checks
each of these on every tested platform before an owner publishes; see
[Release verification](releasing.md#release-workflow-and-publication). Artifact
publication and a v1 tag remain pending until the final-tree Mac/Linux and native
four-surface gates pass, as tracked in the
[v1 readiness plan](../plans/2026-10-06-v1-readiness.md).
