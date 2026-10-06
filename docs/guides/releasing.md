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
reachability and expires within 90 days (see CONTRIBUTING). The locked Rust audit
(`cargo audit` or `cargo deny`) is still an open release task.

Release artifacts need a version matching the tag, an exact reviewed source
commit, supported OS/architecture labels, checksums, build provenance, recovery
verification and a documented installation path. No release is created by this
checklist or by opening its implementation PRs.
