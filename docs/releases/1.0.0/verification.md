# 1.0.0 release verification

Public-safe evidence for the gates in [Release verification](../../guides/releasing.md).
Runtime paths, identifiers and captured values are redacted or synthetic.
Source: the 1.0.0 tree on `main` (merge of #12). The tested binary was built
from `5ffb0f8` with the release workflow's path remapping; later commits change
only scripts and documentation, so the binary is identical.

## Gates

| Gate | Status | Evidence |
| --- | --- | --- |
| Repository policy `--plan` and `--check` on the final `main` | Pending | Re-run after #11, which adds `release.yml` to the reviewed workflows |
| Local suite | Passed | macOS arm64: 354 Rust tests passed (1 ignored), clippy and fmt clean, scripts and docs suites pass. Local Node is 26, outside the supported 22, so CI is the supported-tool record |
| Exact-tree CI, Mac and Linux | Passed | #12's PR CI on its head, whose tree the merge keeps; `main` push CI follows the merge |
| Native evidence, Codex CLI | Passed 2026-10-09 | See below |
| Native evidence, Codex desktop coding | Not rerun on this tree | Last checked 2026-10-05/06 on the compatibility build ([compatibility](../../reference/compatibility.md)); `scripts/native-evidence.py --surface desktop` reruns it |
| Native evidence, Claude Code CLI | Passed 2026-10-09 | See below |
| Native evidence, Claude desktop coding | Not rerun on this tree | As for Codex desktop |
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

## Native coding-agent evidence

`scripts/native-evidence.py` on macOS 27.0.1 (arm64), 2026-10-09, with synthetic
sessions in isolated state, an enrolled software reader and an ephemeral local
Wrangler/D1 collector. Each run required keyless export of every envelope and
encrypted value, tenant and capability rejection, reader decryption of the
prompt and no plaintext canary in local state, D1 or collector output.

| Surface | Agent version | Native hook receipts | Agent's MCP call | Encrypted values | Result |
| --- | --- | --- | --- | --- | --- |
| Claude Code CLI | 2.1.285 | MessageDisplay, PostToolBatch, PostToolUse, PreToolUse, SessionEnd, SessionStart, Stop, UserPromptSubmit (31 registered) | `mcp__gently__list_traces` | 13 | Pass |
| Codex CLI | 0.160.0 | PermissionRequest, PostToolUse, PreToolUse, SessionEnd, SessionStart, Stop, UserPromptSubmit (12 registered) | `mcp__gently__list_traces` | 8 | Pass |

The Codex CLI run used `--dangerously-bypass-hook-trust` for these vetted hooks,
so it does not exercise hook trust. The desktop surfaces were not rerun on this
tree; this release does not claim fresh desktop evidence.
