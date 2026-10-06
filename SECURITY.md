# Security reports

Do not attach real hook payloads, traces, state databases, ciphertext archives,
credentials or private keys to public issues or pull requests. Use invented
reproductions and fixed diagnostics.

## How to report a vulnerability

1. **Private vulnerability reporting (preferred).** If this repository's
   **Security** tab shows **Report a vulnerability**, use it. The report stays
   private between you and the maintainer.
2. **If that button is not shown.** Private reporting is not enabled yet. Open
   a [new issue](https://github.com/JamieAP/gently/issues/new) titled
   `Security contact request`. Leave out the affected component, the
   vulnerability, reproduction steps, logs and any sensitive data, and ask the
   maintainer to arrange a private channel. Share details only after that
   channel is in place.

A public issue with full details is appropriate only for non-sensitive bugs.

Gently is currently pre-v1. Security fixes target current `main`; no older release
maintenance promise exists yet. See
[Security and privacy](docs/concepts/security-and-privacy.md) for the local
storage, encrypted capture and same-user trust boundaries.

Release gates require reviewed changes, Mac/Linux validation, dependency audit,
secret scanning and push protection readback. Client-side disclosure guards
supplement those controls and cannot classify every private value.

The repository readiness check enables private vulnerability reporting and
confirms it by readback before public Actions are enabled. Until then, use the
issue-based contact request in step 2.
