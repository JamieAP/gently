# V1 readiness implementation plan

**Goal:** Ship a credible local-first observational tracer for Codex and Claude Code, in CLI and coding desktop surfaces, without losing queued data or exposing private captures.

**Architecture:** Keep read-only query interfaces, encrypted raw objects, signed tenant policy, local metadata storage and the collector's tenant boundaries. Each release gap receives a focused PR and regression coverage. Repository settings are changed only after their policy is reviewed.

**Tech stack:** Rust, SQLite, TypeScript/Cloudflare D1, Python repository tooling, GitHub Actions.

## Release tasks

- [ ] **P1 — MCP conformance.** [PR #3](https://github.com/JamieAP/gently/pull/3). Negotiate supported protocol versions, implement ping, distinguish protocol errors from tool failures, validate arguments and bound stdio frames. Files: `crates/gently-cli/src/cmd_mcp.rs`, `crates/gently-cli/tests/mcp.rs`. Verify: MCP integration tests, Rust format and clippy.
- [ ] **P1 — Capture health.** [PR #4](https://github.com/JamieAP/gently/pull/4). Persist capture outcomes without payloads, expose last capture and categorized degradation, and report signed-policy expiry without unlocking a reader. Files: hook/status commands, `gently-store` health/schema, CLI health tests. Verify: metadata-only, encrypted, expired/missing policy, size/budget failure paths; existing encrypted schema upgrade.
- [ ] **P1 — Backlog preservation.** [PR #5](https://github.com/JamieAP/gently/pull/5). Preserve metadata by default even when collector auth/network fails; make destructive trimming explicit. Provide bounded metadata quarantine inspection and explicit retry. Files: export command, exporter/store, CLI reference. Verify: over-cap rejected export preserves every row; retry is transactional and summaries omit payloads.
- [ ] **P1 — Bounded trace queries.** [PR #6](https://github.com/JamieAP/gently/pull/6). Add deterministic cursor pages and explicit completion; retain global display bounds and tenant scoping. Update Worker, direct/broker clients and query docs. Verify: large traces, equal timestamps, malformed/cross-trace cursors, complete client reconstruction, response limits.
- [ ] **P1 — Release verification and public policy.** [PR #7](https://github.com/JamieAP/gently/pull/7). Prepare safe owner-operated PR validation and enforceable repository policy; include secret scanning/push protection readback and honest four-surface release evidence. Verify: policy tests, workflow guard tests, local full suite. Applying settings and recording native desktop evidence remain explicit release gates.
- [ ] **P2 — Installation and recovery contract.** [PR #8](https://github.com/JamieAP/gently/pull/8). Document supported dependencies and upgrade/uninstall behavior; provide consistent encrypted-state backup/restore and verify recovery. Define versioned artifacts/checksums and release gates before tagging v1. Verify: live-WAL backup, restore into empty state, incompatible/plaintext schema refusal, preserved user configuration.

- [ ] **P1 — Dependency advisories.** [PR #9](https://github.com/JamieAP/gently/pull/9). Upgrade the supported Worker test/tool stack, preserve independent synthetic D1 fixtures and add a CI dependency audit. Verify: clean lockfile install, Worker tests/type checks, local real CLI/Worker acceptance and advisory readback. The original Worker lock reported 15 development-tool findings (including two critical); the replacement lock reports zero npm advisories as of 2026-10-06.

## Current status

All seven implementation PRs are open and independently based on sanitized `main`.
Component regressions and independent reviews have passed. The dependency PR also
passed synthetic encrypted CLI -> age -> actual Wrangler/workerd/D1 -> CLI/MCP
acceptance. No repository settings, release tags, deployed resources or personal
runtime state were changed by this batch. Checkboxes remain open until acceptance
and merge; open PRs do not establish total native harness parity or v1 readiness.

## Final release gates

- [ ] Combine the reviewed PRs and repeat the full suite on the exact final tree.
- [ ] Record passing macOS and Linux CI with the supported Node/Rust toolchains.
- [ ] Record exact current versions and synthetic final-tree native evidence for Codex CLI/Desktop and Claude Code CLI/Desktop coding surfaces. Ordinary chat/Cowork is outside scope.
- [ ] Review and apply repository protections, including secret scanning and push protection, then verify readback before enabling Actions.
- [ ] Enroll and test an independent recovery reader; verify backup/restore without replacing personal state.
- [ ] Publish reviewed versioned artifacts, checksums/provenance and install/upgrade evidence before a v1 tag.

## Execution order and review

1. Create this task-list PR from sanitized `main`.
2. Implement each focused task on a separate `codex/` branch from `main`; state any dependencies in its PR.
3. Add synthetic regression tests, run component checks, inspect diffs and obtain an independent code review before publication.
4. Commit and push only with a per-command `GENTLY_PUBLIC_REPO_SANITY=1` acknowledgement and the installed disclosure guards. Never include real traces, local runtime configuration, keys or tenant identifiers.
5. Link each implementation PR here. An open PR is not a completed release gate; check items only after acceptance evidence and merge.
6. Run the complete cross-platform and native four-surface release checks against the final combined tree before creating a v1 tag.

## Deferred beyond a local-first v1

Cloud/multi-user hosting needs quotas, rate limits, retention/deletion controls and measured load tests. Writer authentication, broader harness support and a graphical dashboard are separate future work. Operators need an independent recovery reader before relying on a single hardware identity.
