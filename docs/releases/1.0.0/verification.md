# 1.0.0 release verification

Public-safe evidence for the gates in [Release verification](../../guides/releasing.md).
Runtime paths, identifiers and captured values are redacted or synthetic.
**Status: in progress.** The tag waits until every gate below is checked.

## Gates

| Gate | Status | Evidence |
| --- | --- | --- |
| Repository policy `--plan` and `--check` on the final `main` | Pending | Re-run after #11, which adds `release.yml` to the reviewed workflows |
| Final-tree local suite with supported tools | Pending | |
| Final-tree `main` push CI, Mac and Linux | Pending | |
| Native evidence, Codex CLI | Pending | |
| Native evidence, Codex desktop coding | Pending | |
| Native evidence, Claude Code CLI | Pending | |
| Native evidence, Claude desktop coding | Pending | |
| Locked npm audit (`worker`, `docs`) | Passing in CI | One reviewed exception, GHSA-wq5f-xc86-pv6w (`sharp`), expires 2026-11-05 |
| Locked Rust audit | Passing in CI | #10; see below |
| Release artifacts, checksums, provenance, install/upgrade/recovery | Pending | #11's workflow, run on the tag |
| Version, tag and CHANGELOG agree | Pending | This PR sets 1.0.0; `release-check.py version` runs in the release build |

## Rust dependency audit

`scripts/rust-audit.py` with cargo-audit 0.22.2, in the PR #10 CI run
[37923353376](https://github.com/JamieAP/gently/actions/runs/37923353376) on Mac
and Linux and in the `main` push run
[37925415327](https://github.com/JamieAP/gently/actions/runs/37925415327) for
`e8b7bc6`: 0 findings in 408 locked crates against 1296 RustSec advisories
(database `7eebec69c352`, updated 2026-10-09). On the previous `main` (`dedfbb6`)
the gate failed on three findings, fixed in #10 by targeted updates: rustls
0.23.45 (RUSTSEC-2026-0285), quinn-proto 0.11.15 (RUSTSEC-2026-0185) and anyhow
1.0.103 (RUSTSEC-2026-0190, unsound).

## Upgrade and recovery, pre-release check

On macOS arm64, `release-check.py upgrade` with a build of the last pre-1.0 `main`
commit (`dedfbb6`) as the previous version and a build of #11's branch: 6
synthetic envelopes queued by the previous binary survived replacement at the
same path, 1 more was queued by the new binary, a backup restored all 7 into a
fresh namespace, a second restore was refused and uninstall removed both
harness registrations. The release workflow repeats this on both platforms
against the published archives.
