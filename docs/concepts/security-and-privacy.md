# Security & privacy

## What's stored - digests, not content

Spans carry:

* structural metadata - span/trace ids, names, timings, status, `kind`;
* `tool_name`, `tool_use_id`, `permission_mode`;
* **sha-256 digests** (first 8 bytes) plus byte lengths of tool inputs/responses
  and prompts.

So a trace reveals *which* tools ran, *when*, *how big* the I/O was, and *whether*
it errored.

What *is* identifiable, by design: the working directory (`gently.cwd`),
hostname, OS, and session ids. Be aware traces show which projects/paths you ran
in.

Debug mode: `GENTLY_DEBUG=1` writes **full raw payloads** to
`~/.gently/raw/*.jsonl` for schema verification. It's off by default and local
only.

## Transport & authentication

* **In transit:** TLS 1.3 over HTTP/3 (QUIC) or HTTP/2.
* **Endpoint auth:** every route (`/v1/traces`, `/v1/query`, `/v1/whoami`)
  requires `Authorization: Bearer <GENTLY_TOKEN>`, compared in (near) constant
  time; missing/wrong → 401. No unauthenticated route.
* The token is a **single shared bearer secret** stored as a Cloudflare secret on
  the Worker and locally. It is never logged or placed in `wrangler.toml`.

## Data at rest (D1)

Cloudflare D1 is managed SQLite, encrypted at rest on Cloudflare's infrastructure.
It is not directly internet-exposed - reachable only through the bearer-gated
Worker, or via your own Cloudflare account.

## Honest limits

This is a personal/dev-grade tool, not hardened multi-tenant infrastructure:

* one shared token - anyone holding it can read and write all traces;
* the Worker is a public `*.workers.dev` endpoint protected only by that token
  (no IP allowlist, mTLS, Cloudflare Access, or rate limiting by default);
* no retention/TTL - data accumulates until you clean it.

If you need more, front the Worker with Cloudflare Access, add a retention sweep,
or redact `cwd` to a digest.
