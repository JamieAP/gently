# Release verification

A local-first v1 covers observational capture for Codex and Claude Code in CLI
and coding desktop surfaces, with the documented event/lifecycle limits. It does
not establish cloud service capacity or all-session capture completeness.

## Code and repository gates

Run the Rust, Worker, launcher, documentation and real encrypted acceptance checks
from CONTRIBUTING on the final combined tree. Require green Mac and Linux CI on
that same tree. Owner-created same-repository PRs into `main` may run read-only CI;
disallowed actors/authors/forks fail the authorization step before checkout.
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
secret scanning/push protection, sets required Mac/Linux checks and review gates,
and enables Actions only after policy readback. It preserves stronger existing
branch controls or refuses to replace them. A sole-owner administrator bypass
remains documented; an administrator can still override protection.

Creating the policy PR does not apply settings or prove CI ran. Keep release gates
pending until actual readback and exact-tree CI evidence exist. Review all stored
GitHub Apps independently; OAuth cannot enumerate every installation.

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
finding before a v1 tag. Do not run blanket force upgrades. The Worker tooling
audit is an open release task until patched versions and acceptance pass.

Release artifacts need a version matching the tag, an exact reviewed source
commit, supported OS/architecture labels, checksums, build provenance, recovery
verification and a documented installation path. No release is created by this
checklist or by opening its implementation PRs.
