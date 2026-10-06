# Security reports

Do not attach real hook payloads, traces, state databases, ciphertext archives,
credentials or private keys to public issues or pull requests. Use invented
reproductions and fixed diagnostics.

Use GitHub's **Security → Report a vulnerability** if private reporting is
available. If that route is unavailable, request a private contact channel from
the maintainer without describing the vulnerability or sharing sensitive data
in the public request. A public issue is appropriate only for non-sensitive bugs.

Gently is currently pre-v1. Security fixes target current `main`; no older release
maintenance promise exists yet. See the security/privacy documentation for the
local storage, encrypted capture and same-user trust boundaries.

Release gates require reviewed changes, Mac/Linux validation, dependency audit,
secret scanning and push protection readback. Client-side disclosure guards
supplement those controls and cannot classify every private value.

Private vulnerability reporting must be enabled and confirmed by the repository
readiness check before public Actions are enabled. Until that readback is recorded,
use the private-contact fallback above; do not publish vulnerability details.
