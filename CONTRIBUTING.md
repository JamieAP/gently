# Contributing

Small, focused changes and reproducible bug reports are welcome. Describe the
problem, the intended behavior and how to verify the change.

## Repository layout

| Path | Purpose |
| --- | --- |
| `crates/gently-cli` | CLI commands, installation, queries, MCP and waterfall rendering. |
| `crates/gently-harness` | Claude Code and Codex hook adapters. |
| `crates/gently-core` | Span/envelope types and identifiers. |
| `crates/gently-store` | Local SQLite state, outbox, immutable ciphertext and file permissions. |
| `crates/gently-raw` | Age encryption, signed recipient policy, reader identities and contextual binding. |
| `crates/gently-export` | Export transports, delivery and retry behavior. |
| `worker` | Cloudflare Worker, D1 schema and query API. |
| `scripts` | Local collector/export launchers and their tests. |
| `docs` | Setup, concepts, guides and reference. |

See [Architecture](docs/concepts/architecture.md) for how these parts fit together.

## Check a change

Install this checkout's public-repository guards before committing or pushing:

```sh
git config --local core.hooksPath .githooks
chmod +x .githooks/pre-commit .githooks/pre-push
```

Review the staged diff and outgoing commits for public disclosure, including
private customer data, session content and credentials. Then acknowledge that
review on each Git command:

```sh
GENTLY_PUBLIC_REPO_SANITY=1 git commit
GENTLY_PUBLIC_REPO_SANITY=1 git push
```

Both hooks reject an absent acknowledgement, private artifact paths and common
credential patterns. Pre-commit scans the Git index; pre-push scans outgoing
commit snapshots and complete commit/tag objects, including author headers
and data deleted by a later commit. Shallow history and local grafts are refused
because they prevent verification of the original parent graph.
Diagnostics identify the path/rule without printing matched values. The flag
does not bypass the scan. Do not export it in your shell profile or store it in
Git configuration: it acknowledges the review for that command. These local
guards supplement human review; they do not recognize every sensitive value
and Git's client-side hooks are not server-enforced.

From the repository root, run the Rust checks:

```sh
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

For Worker changes:

```sh
cd worker
npm ci
npm test
npm run typecheck
cd ..
```

For local launcher changes:

```sh
python3 -m unittest discover -s scripts -p 'test_*.py'
```

Add regression coverage for behavior changes in the affected component. Use
synthetic hook payloads and a local test collector rather than real session
content or credentials. Documentation-only changes need accurate commands,
examples and links; keep them consistent with CLI help and the shipped source.

## Report a bug

Include:

- Gently version, operating system, and agent name/version.
- Minimal reproduction steps or an invented payload.
- Expected and observed behavior.
- Relevant error text with credentials and identifying details removed.

The [troubleshooting guide](docs/guides/troubleshooting.md) helps distinguish
capture, export and query problems. Do not attach a state database or unreviewed
debug capture: they can contain private content and local paths. Use invented
values for tokens, session IDs, prompts and tool output.
See [Security and privacy](docs/concepts/security-and-privacy.md).

## Keep the scope clear

Keep unrelated formatting, dependency and runtime changes separate. If a hook
mapping changes, update its fixture and [reference](docs/reference/hooks.md).
If a CLI or query interface changes, update help, examples and the relevant
reference. Preserve metadata-only defaults, separate encrypted capture/sync/
resolution opt-ins, tenant/device boundaries and keyless capture/export. Never
use real private keys or credentials in tests. Encrypted schema changes require
reviewed migrations or explicit version refusal
and backup/restore coverage. Preserve supported encrypted state; do not add
plaintext migration paths.

## License

Gently is licensed under [MIT](LICENSE). Dependencies retain their own licenses
and notices.
