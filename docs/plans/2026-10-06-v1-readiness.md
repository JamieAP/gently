# V1 readiness implementation plan

**Goal:** Ship a credible local-first observational tracer for Codex and Claude Code, in CLI and coding desktop surfaces, without losing queued data or exposing private captures.

**Architecture:** Keep read-only query interfaces, encrypted raw objects, signed tenant policy, local metadata storage and the collector's tenant boundaries. Each release gap receives a focused PR and regression coverage. Repository settings are changed only after their policy is reviewed.

**Tech stack:** Rust, SQLite, TypeScript/Cloudflare D1, Python repository tooling, GitHub Actions.

**Canonical gate list:** [Release verification](../guides/releasing.md) (`docs/guides/releasing.md`, merged into `main` with #7). That guide defines the release gates, the supported build tools and the evidence location, `docs/releases/<version>/verification.md`. This file maps the gates to PRs, records their order and tracks status. It does not restate the gate criteria.

## Release tasks

- [x] **P1 — MCP conformance.** [PR #3](https://github.com/JamieAP/gently/pull/3), merged ([landing status](#landing-status)). Negotiate supported protocol versions, implement ping, distinguish protocol errors from tool failures, validate arguments and bound stdio frames. Files: `Cargo.lock`, `crates/gently-cli/Cargo.toml`, `crates/gently-cli/src/{cmd_mcp,query_broker,query_client}.rs`, `crates/gently-cli/tests/{mcp,query_broker}.rs`, `docs/guides/querying-and-mcp.md`, `scripts/acceptance-local.py`. Verify: MCP integration tests, Rust format and clippy.
- [x] **P1 — Capture health.** [PR #4](https://github.com/JamieAP/gently/pull/4), merged ([landing status](#landing-status)). Persist capture outcomes without payloads, expose last capture and categorized degradation, and report signed-policy expiry without unlocking a reader. Files: `README.md`, `crates/gently-cli/src/{cmd_hook,cmd_status,local_raw,main}.rs`, `crates/gently-cli/tests/hook.rs`, `crates/gently-raw/src/{lib,tests}.rs`, `crates/gently-store/src/{health,lib}.rs`, `docs/concepts/reliability.md`, `docs/guides/troubleshooting.md`, `docs/reference/cli.md`. Verify: metadata-only, encrypted, expired/missing policy, size/budget failure paths; existing encrypted schema upgrade (merge after #8; see [merge order](#merge-order-and-cross-pr-interactions)).
- [ ] **P1 — Backlog preservation.** [PR #5](https://github.com/JamieAP/gently/pull/5), merging ([landing status](#landing-status)). Preserve metadata by default even when collector auth/network fails; make destructive trimming explicit. Provide bounded metadata quarantine inspection and explicit retry. Files: `CHANGELOG.md`, `README.md`, `crates/gently-cli/src/{cmd_export,cmd_hook,cmd_init,cmd_quarantine,config,main}.rs`, `crates/gently-cli/tests/{backlog,quarantine,query_broker}.rs`, `crates/gently-export/src/lib.rs`, `crates/gently-store/src/{health,lib,quarantine}.rs`, `docs/concepts/reliability.md`, `docs/getting-started/configuration.md`, `docs/guides/{querying-and-mcp,troubleshooting}.md`, `docs/reference/{cli,compatibility}.md`. Verify: over-cap rejected export preserves every row; retry is transactional and summaries omit payloads.
- [ ] **P1 — Bounded trace queries.** [PR #6](https://github.com/JamieAP/gently/pull/6), merging ([landing status](#landing-status)). Add deterministic cursor pages and explicit completion; retain global display bounds and tenant scoping. Files: `crates/gently-cli/src/{cmd_mcp,collector,query_broker,query_client}.rs`, `crates/gently-cli/tests/{trace_pages,waterfall}.rs`, `docs/guides/querying-and-mcp.md`, `docs/reference/worker.md`, `worker/schema.sql`, `worker/src/{d1,index}.ts`, `worker/test/{schema,security.test,worker.test}.ts`. Verify:
  - large traces, equal timestamps, malformed/cross-trace cursors, complete client reconstruction and response limits;
  - per-page D1 work is bounded independently of trace size (an index seek with no sort of the whole trace), so a full read costs D1 work linear in trace size;
  - every row that ingest accepts is readable through both the paged and unpaged paths;
  - mixed-version rollout: an older CLI or watcher against the new Worker, and a new CLI against an older watcher, keep working or fail with a documented limit;
  - generation-based change detection: a write that could move a row behind a cursor advances the trace generation, the next page returns 409, and `complete: true` never hides a row stored for the whole read.
- [x] **P1 — Release verification and public policy.** [PR #7](https://github.com/JamieAP/gently/pull/7), merged ([landing status](#landing-status)). Prepare safe owner-operated PR validation and enforceable repository policy; include secret scanning/push protection and private vulnerability reporting readback, and honest four-surface release evidence. Files: `.github/workflows/ci.yml`, `SECURITY.md`, `docs/SUMMARY.md`, `docs/guides/releasing.md`, `scripts/github-public-readiness.py`, `scripts/test_github_public_readiness.py`. Verify: policy tests, workflow guard tests, local full suite. Applying settings and recording native desktop evidence remain release gates.
- [x] **P1 — Dependency advisories.** [PR #9](https://github.com/JamieAP/gently/pull/9), merged ([landing status](#landing-status)). Upgrade the supported Worker test/tool stack, preserve independent synthetic D1 fixtures and add a CI npm audit of the `worker` and `docs` locks. Files: `.github/workflows/ci.yml`, `CONTRIBUTING.md`, `README.md`, `docs/audit-allowlist.json`, `docs/getting-started/{local-collector,quickstart}.md`, `docs/guides/encrypted-raw-validation.md`, `docs/package.json`, `docs/reference/worker.md`, `scripts/{acceptance-local.py,collector-local,npm-audit.py,test_npm_audit.py}`, `worker/{audit-allowlist.json,package-lock.json,package.json,tsconfig.json,vitest.config.ts}`, `worker/test/{env.d,reset,security.test,worker.test}.ts`. Verify: clean lockfile install, Worker tests/type checks, local real CLI/Worker acceptance and advisory readback. Before #9, `main`'s Worker lock reported 15 development-tool findings (including two critical). With #9's lock, now on `main`, the CI audit fails on none; one high advisory (GHSA-wq5f-xc86-pv6w, `sharp`) is a reviewed exception that expires on 2026-11-05.
- [ ] **P1 — Rust dependency audit.** No PR yet. Owner: unassigned. The canonical list requires auditing locked Rust and Node dependencies; #9's CI audit covers only npm, and neither `main` nor the #5 and #6 heads run `cargo audit` or `cargo deny`. Verify: a locked Rust advisory check in CI, with every unresolved finding fixed or reviewed.
- [ ] **P1 — Release artifacts, checksums and provenance.** No PR yet. Owner: unassigned. `main` and the #5 and #6 heads have only the `ci.yml` and `docs.yml` workflows, and neither builds artifacts. Verify: a release workflow that produces OS/architecture-labelled artifacts from an exact reviewed commit, with checksums, build provenance and an install/upgrade check.
- [ ] **P1 — Version bump and CHANGELOG cut.** No PR yet. Owner: unassigned. The workspace version is still `0.1.0` (`Cargo.toml`) and `CHANGELOG.md` has only an `[Unreleased]` pre-1.0 section; the canonical list requires the artifact version to match the tag. Verify: tag, workspace version and CHANGELOG release heading agree.
- [x] **P2 — Installation and recovery contract.** [PR #8](https://github.com/JamieAP/gently/pull/8), merged ([landing status](#landing-status)). Document supported dependencies and upgrade/uninstall behavior; provide consistent encrypted-state backup/restore and verify recovery. Document the versioned release contract (artifacts themselves are the task above). Files: `CONTRIBUTING.md`, `Cargo.toml`, `crates/gently-cli/src/{cmd_init,cmd_state,main}.rs`, `crates/gently-cli/tests/{init,recovery}.rs`, `crates/gently-store/src/{backup,lib,private_fs}.rs`, `docs/SUMMARY.md`, `docs/guides/installation-and-recovery.md`, `docs/reference/cli.md`. Verify: live-WAL backup, restore into empty state, incompatible/plaintext schema refusal, preserved user configuration.

## Current status

#3, #4, #7, #8 and #9 are merged into `main`, and `main`'s push CI is green on Mac and Linux for every merge that has a run. #5 and #6 are merging; this PR lands last. The release tasks without a PR above are still open. Of the release gates below, only step 1 (merging #7) is closed. The repository has no tags or releases (GitHub API, 2026-10-06T18:59Z).

### Landing status

Read at 2026-10-06T18:59Z from `git log origin/main`, `gh pr view N --json mergeCommit` and the Actions runs API. The PRs landed in the order #7 → #9 → #8 → #3 → #4, then #5 → #6 (planned: #7 → #9 → #8 → #4 → #5 → #3 → #6).

| PR | State | Merge commit (merged, UTC) | `code` CI |
| --- | --- | --- | --- |
| #7 | Merged | `2088d01` (18:28:50) | `main` push: no run, predates Actions being enabled |
| #9 | Merged | `bd4b49d` (18:29:21) | `main` push: no run, predates Actions being enabled |
| #8 | Merged | `13e9277` (18:29:33) | `main` push: no run, predates Actions being enabled |
| #3 | Merged | `2d0c5f0` (18:41:59) | `main` push: [green](https://github.com/JamieAP/gently/actions/runs/37513313197), Mac and Linux |
| #4 | Merged | `51c8b48` (18:54:18) | `main` push: [green](https://github.com/JamieAP/gently/actions/runs/37514890682), Mac and Linux |
| #5 | Merging, head `065c285` | — | PR: [in progress](https://github.com/JamieAP/gently/actions/runs/37515316469) |
| #6 | Merging, head `a65ffbd` | — | PR: no run; the head does not contain `main` |

- The repository's first workflow run started at 18:42:04Z, after #7, #9 and #8 merged. The `2d0c5f0` and `51c8b48` runs test trees that contain all three. Every validation step passed on both OSes, including the real CLI/Wrangler/D1 acceptance and the npm audit; the only skipped step is the owner-authorization guard, which runs only to reject a disallowed actor.
- #4 also had green PR CI ([run](https://github.com/JamieAP/gently/actions/runs/37513855157) on `3cd9825`). #3, #7, #8 and #9 have no PR CI run.
- `docs` workflow: the `2d0c5f0` build passed, and its deploy was cancelled when the `51c8b48` run superseded it in the `pages` concurrency group. The `51c8b48` build passed, and its deploy is waiting for the `github-pages` reviewer.

Repository policy, read from the GitHub API at 2026-10-06T18:59:50Z:

- Actions (`actions/permissions`): enabled, `allowed_actions: selected`, SHA pinning required. The default workflow token is `read` and cannot approve PR reviews. Fork PR runs need approval for all external contributors.
- `main` protection: strict required checks `validate (macos-latest)` and `validate (ubuntu-latest)`, bound to the GitHub Actions app (15368); 1 approving review with code-owner review and stale-review dismissal; conversation resolution required; force pushes and deletions blocked. `enforce_admins` is false, which is the documented sole-owner administrator bypass. Signed commits and linear history are not required.
- `security_and_analysis`: secret scanning and push protection enabled. Non-provider patterns, validity checks and Dependabot security updates disabled.
- Private vulnerability reporting: enabled.
- `github-pages` environment: required reviewer `JamieAP`, `can_admins_bypass: false`, deployments limited to `main`.

## Combined local validation

**Superseded.** #3, #4, #7, #8 and #9 have merged, and #5 has new commits since this run. For merged code, the `main` push CI under [landing status](#landing-status) is the current evidence. The original record follows.

Recorded on 2026-10-06 for these heads: #3 `60e8188`, #4 `17ee224`, #5 `207e5a1`, #6 `a65ffbd`, #7 `04eb8b7`, #8 `cf1b0a8`, #9 `1655c6d`, all on `63aa64f`. **This record goes stale on the next push to any of these branches or to `main`.**

An octopus merge of the seven heads stops on a conflict. Merging them in the order below, with the resolutions under [merge order](#merge-order-and-cross-pr-interactions), gives source tree `370ef05480a7609031bef4f8280eb55b9d1efa05`. On that tree:

- `cargo fmt --all -- --check` and `cargo clippy --locked --all-targets -- -D warnings` pass;
- `cargo test --locked --no-fail-fast`: 352 passed, 0 failed, 1 ignored (the `lock_probe_child` subprocess helper) across 24 test targets;
- Worker: `npm ci --ignore-scripts`, `npm run audit`, `npm test` (72 passed in 2 files) and `npm run typecheck` pass; the audit fails on no advisory, with the reviewed `sharp` exception noted above;
- `python3 -m unittest discover -s scripts -p 'test_*.py'`: 99 tests OK, including #7's check of the shipped workflows;
- docs: `npm ci --ignore-scripts` (0 vulnerabilities), 9 tests passed and a 20-page build (this PR is not in the merge).

Tools: rustc 1.99.0 (b940084d7 2026-09-28), cargo 1.99.0 (5f94df478 2026-08-27), Node.js 26.10.0, npm 11.19.1, Python 3.14.8. Node 26 and Python 3.14 are outside the [supported build tools](../guides/releasing.md#supported-build-tools-and-evidence), so this run is integration evidence, not release evidence. `scripts/acceptance-local.py` (real Wrangler/workerd/D1) and `github-public-readiness.py --check` were not run: `--check` has no local-only mode and reads live repository settings and the workflows on remote `main`.

## Release gate order

Gate criteria live in the [canonical list](../guides/releasing.md). PR CI needed steps 1–3, so they came first. Actions and the required Mac/Linux checks are now enabled (see [landing status](#landing-status)).

- [x] 1. Merge #7 on local evidence (`2088d01`); it brings the `pull_request` trigger, the readiness script and the canonical list.
- [ ] 2. Review the repository policy: `github-public-readiness.py --plan`, review installed GitHub Apps, then `--check --reviewed-main-sha <full main sha> --apps-reviewed`.
- [ ] 3. Apply and read back repository protections with `--apply` and the same reviewed `main` commit. It disables Actions first, enables and reads back secret scanning, push protection and **private vulnerability reporting**, binds the required Mac/Linux checks, sets review gates and enables Actions only after policy readback. Record the readback. The live settings are recorded under [landing status](#landing-status). This file holds no `--check` or `--apply` output, so steps 2–3 stay open until that output is recorded.
- [ ] 4. Merge the remaining PRs in the order below, each with green Mac/Linux PR CI. #9 and #8 merged before Actions was enabled, and #3 has no PR CI run; their tree is covered by green `main` push CI. #4 merged with green PR CI. #5 and #6 are merging.
- [ ] 5. On the exact final `main` tree: the full local suite and the `main` push CI with the [supported build tools](../guides/releasing.md#supported-build-tools-and-evidence) (stable Rust, Node.js 22 at least 22.12, Python 3.12), with exact versions recorded.
- [ ] 6. [Native coding-agent evidence](../guides/releasing.md#native-coding-agent-evidence) for Codex CLI/Desktop and Claude Code CLI/Desktop coding surfaces. Ordinary chat/Cowork is outside scope.
- [ ] 7. [Dependency and artifact gates](../guides/releasing.md#dependency-and-artifact-gates): npm audit (#9, in CI on `main`), Rust audit (no PR yet), artifacts/checksums/provenance (no PR yet), version bump and CHANGELOG cut (no PR yet).
- [ ] 8. Save the evidence for steps 3–7 in `docs/releases/<version>/verification.md`, then tag.

## Merge order and cross-PR interactions

Merge order: #7 → #9 → #8 → #4 → #5 → #3 → #6. The landed order was #7 → #9 → #8 → #3 → #4, then #5 → #6 (see [landing status](#landing-status)). This PR lands last.

- **#7 before #9.** #9 builds on #7's CI shape; both edit `.github/workflows/ci.yml` and merge cleanly as text. Whichever merges second updates the `releasing.md` sentence that says the Worker tooling audit is an open release task. This PR makes that update: the sentence now describes the CI npm audit with reviewed, expiring exceptions and keeps the Rust audit open.
- **#8 before #4.** #4 adds a `capture_health` table without changing the schema version. Before #8, `main`'s `CONTRIBUTING.md` said pre-public schema changes require an explicit reset; #8 changed that to reviewed migrations or explicit version refusal, which is what `main` now says.
- **#4 after #8: conflict** in `crates/gently-store/src/lib.rs` (re-exports). Keep both `pub use backup::…` and `pub use health::{CaptureHealth, CaptureOutcome, Health}`. Resolved in #4's merge of `main` (`3cd9825`), and `main` has both.
- **#5 after #4 and #8: conflicts** in `crates/gently-store/src/health.rs` (keep #4's `CaptureOutcome`, `capture_record` and `capture_snapshot` with #5's typed `outbox_quarantine`) and the command table in `docs/reference/cli.md` (list `init`, `uninstall`, `status`, `state`, `raw`, `quarantine`). A third break has no text conflict: #8's test in `crates/gently-store/src/backup.rs` passes a string to `outbox_quarantine`, which #5 changes to take `QuarantineReason`, so it must pass `QuarantineReason::InvalidJson`. #5's head `065c285` contains `main` and passes it.
- **#6 after #3: conflicts** in `crates/gently-cli/src/cmd_mcp.rs` (three hunks) and `crates/gently-cli/src/query_broker.rs`. Keep #3's `isError: false` result with #6's `bounded_text`, and return #6's oversized-result error as a new fixed `ToolFailure` variant, since #3's `tool_failure` would otherwise replace it with the generic collector message. Keep `additionalProperties: false` on `get_trace` with #6's description, and keep both PRs' test modules. In `query_broker.rs`, keep #3's `Reply::decode` and move #6's restoration of `UnsupportedQueryParameter` and `TraceChanged` into `relayed`, under the `WatcherRelayed` context.
- **#6 after #9: conflicts** in `worker/test/security.test.ts` and `worker/test/worker.test.ts`. Keep #9's per-test `resetDatabase`, and keep #6's `applySchema` import where its idempotency test uses it. #9's `worker/test/reset.ts` splits `schema.sql` at each semicolon before a line break, which would cut #6's `CREATE TRIGGER` bodies apart; it must call #6's `applySchema` instead.
- **#6 deployment.** Reapply `worker/schema.sql` to D1 before deploying the Worker, then upgrade the CLI and restart `gently export --watch --serve-queries` (#6's `docs/reference/worker.md`, "Rollout order").
- **Node.js wording.** The supported version is Node.js 22, at least 22.12. #9's `CONTRIBUTING.md` says "22.12 or later", which admits 24+, and #8's installation guide says "Node.js 22". Align both with `releasing.md` when they merge. Both are merged, and both sentences still read as described.

With these resolutions the merged tree was green (see [combined local validation](#combined-local-validation)). #4's and #5's resolutions are on their branches. #6's head `a65ffbd` does not contain `main`, so its resolutions against #3 and #9 are not on its branch yet.

## Execution order and review

1. Create this task-list PR from sanitized `main`.
2. Implement each focused task on a separate `codex/` branch from `main`; state any dependencies in its PR.
3. Add synthetic regression tests, run component checks, inspect diffs and obtain an independent code review before publication.
4. Commit and push only with a per-command `GENTLY_PUBLIC_REPO_SANITY=1` acknowledgement and the installed disclosure guards. Never include real traces, local runtime configuration, keys or tenant identifiers.
5. Link each implementation PR here. An open PR is not a completed release gate; check items only after acceptance evidence and merge.
6. Follow the [release gate order](#release-gate-order) against the final combined tree before creating a v1 tag.

## Deferred beyond a local-first v1

Cloud/multi-user hosting needs quotas, rate limits, retention/deletion controls and measured load tests. Writer authentication, broader harness support and a graphical dashboard are separate future work. Operators need an independent recovery reader before relying on a single hardware identity; that is an operator step, not a v1 release gate, and the multi-reader procedure is already documented in [encrypted raw values](../guides/encrypted-raw-values.md).
