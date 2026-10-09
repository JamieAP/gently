# Release verification

A local-first v1 covers observational capture for Codex and Claude Code in CLI
and coding desktop surfaces, with the documented event/lifecycle limits. It does
not establish cloud service capacity or all-session capture completeness.

## Code and repository gates

Run the Rust, Worker, launcher, documentation and real encrypted acceptance checks
from CONTRIBUTING on the final combined tree. Require green Mac and Linux CI on
that same tree. Owner-created same-repository PRs into `main` may run read-only CI;
the reviewed workflow rejects disallowed actors/authors/forks before checkout.
That step is defense in depth: `pull_request` loads workflow changes from the PR
merge ref. A fork can rewrite it, so repository approval for **all external
contributors**, read-only tokens, no stored credentials and workflow review are
the actual boundaries. Never approve a fork run that changes `.github` until those
changes are reviewed as executable code. A green PR check is insufficient release
evidence; require the final reviewed `main` push checks.
`pull_request_target` is excluded; required validation jobs are never skipped
as an authorization mechanism. Workflows use pinned
GitHub-owned actions, avoid stored secrets, and never persist checkout credentials.

Review the repository policy before applying it:

```sh
python3 scripts/github-public-readiness.py --plan
python3 scripts/github-public-readiness.py --check \
  --reviewed-main-sha REVIEWED_FULL_MAIN_COMMIT --apps-reviewed
```

After that policy is merged and explicitly approved for application, the owner
can use `--apply` with the same reviewed full main commit. The script disables
Actions first, checks the workflow/access inventory, enables and reads back
secret scanning/push protection and private vulnerability reporting, binds
required Mac/Linux checks to GitHub Actions and sets review gates,
and enables Actions only after policy readback. It preserves stronger existing
branch controls or refuses to replace them. A sole-owner administrator bypass
remains documented; an administrator can still override protection.

Creating the policy PR does not apply settings or prove CI ran. Keep release gates
pending until actual readback and exact-tree CI evidence exist. Review all stored
GitHub Apps independently; OAuth cannot enumerate every installation.

## Supported build tools and evidence

The supported build environment is current stable Rust/Cargo with rustfmt/clippy,
Node.js 22 at its current patch (at least 22.12), npm from that distribution, and
Python 3.12. No lower Rust MSRV is promised. CI installs those channels and records
exact `rustc`, `cargo`, Node, npm and Python versions in each run. Release evidence
must retain those versions alongside its source commit; a floating channel name
alone does not reproduce a build.

Keep public-safe evidence in `docs/releases/<version>/verification.md`, linked to
exact-commit CI runs and synthetic/native acceptance results. This guide is the
canonical release gate list. Check a gate only when that evidence exists.

## Native coding-agent evidence

For each release candidate, record a public-safe table with the exact release
commit, OS, agent CLI version, desktop app/build version, date and result:

| Surface | Required fresh evidence |
| --- | --- |
| Codex CLI | Trusted hooks, representative lifecycle/tool events, receipts and MCP query |
| Codex desktop coding | Same encrypted pipeline with desktop environment and trust behavior |
| Claude Code CLI | Representative observational events, silent hook output, encrypted query |
| Claude desktop coding | Actual coding surface capture plus MCP query; excludes Chat/Cowork |

Use synthetic sessions in isolated state and an ephemeral local collector.
Verify keyless ciphertext capture/export, enrolled-reader decryption, plaintext
canary absence on disk, namespace rejection, outage/restart retention and policy
expiry. Redact runtime paths, IDs and values before any public evidence is saved.
A CLI/schema fixture pass does not prove native desktop behavior. Earlier evidence
against a different tree does not close the release-candidate gate.

## Dependency and artifact gates

Audit locked Rust and Node dependencies, including development/test tooling.
Document advisory reachability and fix or explicitly review every unresolved
finding before a v1 tag. Do not run blanket force upgrades. CI audits the locked
Worker and docs npm dependencies and fails on any advisory without a reviewed
exception in that project's `audit-allowlist.json`; each exception records its
reachability and expires within 90 days (see CONTRIBUTING). CI also audits the
locked Rust dependencies with a pinned cargo-audit (`scripts/rust-audit.py`) and
fails on any RustSec vulnerability or warning without a reviewed exception in the
root `audit-allowlist.json`, under the same expiry rules. Release evidence records
the advisory database commit each audit ran against.

Release artifacts need a version matching the tag, an exact reviewed source
commit, supported OS/architecture labels, checksums, build provenance, recovery
verification and a documented installation path. No release is created by this
checklist or by opening its implementation PRs.

## Release workflow and publication

`.github/workflows/release.yml` builds the artifacts. It runs only when the owner
dispatches it on a `v*` tag; the readiness policy rejects any other trigger, an
unguarded job, or a write permission beyond the attestation job's OIDC token and
attestations. No job can write repository contents, so publishing stays an owner
step after verification.

Tag the reviewed `main` commit that carries the release evidence, push the tag,
then dispatch the workflow on it:

```sh
git tag -a vX.Y.Z -m "Gently X.Y.Z" REVIEWED_FULL_MAIN_COMMIT
git push origin vX.Y.Z
gh workflow run release.yml --repo JamieAP/gently --ref vX.Y.Z
```

The workflow has three jobs:

- **build**, on `macos-latest` (`aarch64-apple-darwin`) and `ubuntu-latest`
  (`x86_64-unknown-linux-gnu`): `scripts/release-check.py version` requires the tag,
  workspace version and newest dated CHANGELOG heading to agree, and the build
  host to match the target label. A locked release build remaps the checkout,
  Cargo and rustup paths, and `package` refuses a binary that still embeds them.
  The archive, `gently-X.Y.Z-TARGET.tar.gz`, holds `gently`, `LICENSE`,
  `README.md` and `CHANGELOG.md` with fixed owners, modes and timestamps.
- **attest**: writes `SHA256SUMS` and records GitHub build provenance for every
  archive with `actions/attest-build-provenance`.
- **verify**, on both platforms: `gh attestation verify` against this workflow and
  tag; `install` checks the archive against `SHA256SUMS` and its exact member list;
  `upgrade` installs the previous version (the latest earlier tag, or for the first
  release the last pre-1.0 `main` commit) at a stable path, queues synthetic events,
  replaces the binary in place, re-runs `init`, then backs up, restores and
  uninstalls; and the real CLI/Wrangler/D1 acceptance runs against the released
  binary.

When every job passes, download the verified bundle, check it locally and publish:

```sh
gh run download RUN_ID --repo JamieAP/gently --name release --dir release-X.Y.Z
cd release-X.Y.Z
shasum -a 256 -c SHA256SUMS
gh attestation verify gently-X.Y.Z-aarch64-apple-darwin.tar.gz --repo JamieAP/gently \
  --signer-workflow JamieAP/gently/.github/workflows/release.yml --source-ref refs/tags/vX.Y.Z
gh release create vX.Y.Z --repo JamieAP/gently --verify-tag --title "Gently X.Y.Z" \
  --notes-file NOTES.md gently-X.Y.Z-*.tar.gz SHA256SUMS
```

`NOTES.md` is the version's CHANGELOG section. Record the workflow run, digests
and attestation links in `docs/releases/X.Y.Z/verification.md`.
