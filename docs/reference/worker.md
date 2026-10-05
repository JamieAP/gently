# Collector and Worker

The included collector is a TypeScript Cloudflare Worker backed by D1. It can run
locally through Wrangler or be deployed to Cloudflare. CLI and MCP queries read
this collector, rather than the local outbox.

## Authentication

Every route requires `Authorization: Bearer <token>` and exactly one
`tenant_id` query parameter. The Worker's `GENTLY_HOSTS` secret is a JSON array
of `{token, tenant_id, device_id, capabilities}` records; no shared
`GENTLY_TOKEN` server fallback is supported. A missing, invalid or ambiguous
credential/configuration returns `401`; a missing/invalid tenant returns `400`;
a different tenant or missing capability returns `403`.

`ingest` permits metadata/ciphertext writes; `read` permits metadata/ciphertext
queries. Tenant ownership comes from the authenticated principal, and every
D1 operation is tenant scoped. Capture device claims are authenticated as
well: missing resource namespace/device attributes are injected from the
principal; conflicting or duplicate resource/span claims are rejected.

Responses use `Cache-Control: no-store` and `X-Content-Type-Options: nosniff`.
There is no automatic credential expiry, account provisioning or retention.
See [security and privacy](../concepts/security-and-privacy.md).

## Endpoints

Every path below also requires `tenant_id=TENANT` in its query string.

| Method and path | Capability | Purpose |
| --- | --- | --- |
| `POST /v1/traces` | ingest | Flatten metadata OTLP/JSON and merge spans |
| `GET /v1/query?op=traces` | read | Trace/session summaries |
| `GET /v1/query?op=trace&trace_id=TRACE_ID` | read | One trace, with effective bounds |
| `GET /v1/query?op=spans` | read | Limited, filtered span search |
| `GET /v1/query?op=stats` | read | Tenant-wide tool rollups |
| `POST /v1/raw-values` | ingest | Store an immutable encrypted raw object |
| `GET /v1/raw-values/RAW_REF` | read | Fetch that tenant's opaque ciphertext object |
| `GET /v1/whoami` | authenticated | Protocol probe for the request |

Successful ingest returns:

```json
{"partialSuccess":{},"httpProtocol":"HTTP/3"}
```

`httpProtocol` comes from request metadata and can be `"unknown"` in environments
without it. `/v1/whoami` returns only that field. It does not identify a user or
session and is unrelated to `gently whoami --pane`.

Unsupported methods, paths and query operations return `404` after auth and
tenant checks. Invalid JSON or guarded payloads return a safe `400`; actual
request bytes over 1 MiB or metadata requests over 32 span reports return
`413`. The span limit bounds upserts and ownership checks within D1's Free-plan
[per-invocation query budget](https://developers.cloudflare.com/d1/platform/limits/).
Other unhandled shape/database errors
return a generic `500`. Empty span envelopes can succeed; the handler does not
validate every OTLP feature or return detailed rejected-span counts.

### Example query

With the collector URL configured and a token supplied to the process:

```sh
gently traces --harness codex --limit 5 --order last_activity --json
```

The CLI reads `GENTLY_TOKEN` from its environment and adds the bearer header.
For a custom client, the corresponding request is:

```http
GET /v1/query?tenant_id=personal&op=traces&harness=codex&limit=5&order=last_activity HTTP/1.1
Authorization: Bearer <token supplied by the client>
```

Read credentials inside the client rather than expanding them into command
arguments. Use the [CLI](cli.md) for tables and waterfalls, or
[MCP](../guides/querying-and-mcp.md) for agent queries.

## Query parameters

| Operation | Parameters |
| --- | --- |
| `traces` | `session_id`, `harness`, `since`, `until`, `limit`, `order` |
| `trace` | `trace_id` |
| `spans` | `trace_id`, `session_id`, `harness`, `tool_name`, `name`, `status`, `kind`, `since`, `until`, `limit`, `order` |
| `stats` | None |

Text filters are exact matches. `since` and `until` are inclusive bounds on raw
span start, supplied as decimal Unix nanosecond strings. In `traces`, filtering
happens before grouping: summaries then count and bound only matching spans.
Start filters and sorting compare stored text, so differing digit lengths or
leading zeros can produce ordering that differs from numeric time. Use the
canonical decimal timestamps emitted by Gently's hooks.

`traces` and `spans` default to 50 rows and cap a requested limit at 1,000.
Missing, non-numeric, or non-positive limits use 50. There is no cursor or offset.

Trace ordering accepts `start_desc` (default), `start_asc`, `last_activity`,
`last_activity_desc`, and `last_activity_asc`. `last_activity` is an alias for
descending activity. Span search accepts `start_desc` (default) and `start_asc`.
Unknown order values fall back to descending start; ties have no explicit
secondary sort key. Numeric filters `status` and `kind` use decimal codes.
Unparseable numeric filters are ignored rather than rejected.

## Query response fields

Queries return JSON arrays. Nanosecond timestamps are decimal strings and IDs
are hex strings in normal Gently-generated rows. Keep timestamps as strings
through JavaScript clients.

| Result | Fields |
| --- | --- |
| Trace summary | `trace_id`, `session_id`, `harness`, `start`, `last_activity`, `span_count`, `error_count` |
| Span row | `span_id`, `trace_id`, `parent_span_id`, `name`, `kind`, `start_unix_nano`, `end_unix_nano`, `status`, `session_id`, `harness`, `tool_name`, `tool_use_id`, `attrs_json`, `resource_json`, `ingested_unix_nano` |
| Single-trace additions | `effective_start_unix_nano`, `effective_end_unix_nano` |
| Tool stats | `tool_name`, `span_count`, `error_count`, `avg_duration_ms` |

Optional context and end fields can be null. `attrs_json` and `resource_json`
are strings encoding OTLP key/value arrays, not embedded JSON arrays. The Worker
lifts session and harness fields from resource attributes, with span overrides
when supplied. It lifts tool name and tool-use ID from span attributes. Lifted
span keys are removed from `attrs_json`; resource attributes remain intact.

Trace `error_count` counts status-2 spans with a non-null `tool_name`, excluding
failed turns and roots. Tool statistics also count only tool rows. Their average
uses raw start/end values for rows with an end, not effective bounds; it is a
floating-point mean, not a percentile or exact nanosecond calculation. A missing
mean is null. `last_activity` is maximum raw end, falling back to start for a row
without an end, rather than last ingestion time.

The typed CLI/MCP rows omit `ingested_unix_nano`. Their span type includes
optional effective fields, serialized as null when absent from a span search.

### Effective bounds

`op=trace` calculates display bounds without changing stored raw timestamps:

- A row with no parent uses the minimum start and maximum end-or-start across
  the entire trace.
- Another row's effective start is the minimum of its own start and its direct
  children's starts.
- If that row has a stored end different from its start, it keeps that end.
  Otherwise, its effective end is the maximum direct-child end-or-start, or its
  own start when it has no children.

This calculation is one level deep, not a recursive envelope of arbitrary
nested descendants. `op=spans` does not calculate effective bounds. See the
[trace model](../concepts/trace-model.md#effective-bounds) for rendering context.

## Ingest and repeated span updates

Gently can report the same span on open, close, and session resume. The Worker
uses `(tenant_id, span_id)` as the primary key and preserves the earliest reported start and
latest reported end-or-start. Status, name, parent, and attribute fields come
from the report whose end-or-start is latest. Equal ending timestamps favor
the incoming report, so conflicting metadata at a tie depends on delivery order.
Changing a span's trace ID returns `409`, including conflicts within a single
request; repeated reports for the same logical span remain valid. Each row has an authoritative immutable
`source_device_id` from authentication. A different device cannot update that
span, even within the same tenant (`409`); moving capture to another device
requires a fresh span ID. Cross-device parent links within a tenant are allowed.
Guarded upserts prevent an ownership race from overwriting another host's row.

Known raw-content aliases and all `.sha256` attributes are rejected rather than
persisted in metadata. Byte lengths and random `.raw_ref` pointers are allowed.
This prevents accidental uploads from old Gently producers; arbitrary external
metadata strings cannot be proven free of private content.

Timestamp strings preserve the received representation, but merge comparisons
and effective bounds cast them to SQLite signed integers. The implementation
assumes values within that range; it does not validate every incoming OTLP
field or support arbitrary unsigned-64 timestamp comparisons correctly.

The ingest log records the request protocol and span count, not payload content.
The response's empty `partialSuccess` object does not report rejected-span counts.

## D1 schema (`worker/schema.sql`)

The `spans` table keys rows by `(tenant_id, span_id)` and stores immutable
capture-device ownership. Indexes start with tenant and cover trace, session,
start, tool and common filters. Queries use explicit metadata projections;
internal storage columns and raw ciphertext are not automatically included.

`raw_values` keys immutable envelopes by `(tenant_id, raw_ref)`, with capture
device, key epoch and creation time. D1 stores bounded ciphertext directly;
no R2 bucket or decryption broker is provisioned. The new schema is pre-public:
initialize a fresh database after an explicit reset of disposable old state,
rather than applying it as a migration. See [schema.sql](https://github.com/JamieAP/gently/blob/main/worker/schema.sql).

## Encrypted raw object contract

An upload has exactly this shape (ciphertext and IDs here are placeholders):

```json
{
  "version": 1,
  "context": {
    "tenant_id": "personal",
    "device_id": "mac-main",
    "key_epoch": 1,
    "raw_ref": "0123456789abcdef0123456789abcdef",
    "session_id": "example-session",
    "harness": "codex",
    "event": "UserPromptSubmit"
  },
  "ciphertext_b64": "STANDARD_BASE64_OF_BINARY_AGE_V1_CIPHERTEXT"
}
```

Extra envelope/context properties are rejected. Tenant/device must match the
upload principal. Namespace IDs use 1–64 ASCII letters, digits, dashes or
underscores. References are 32 lowercase hex characters; key epochs are
positive safe integers. Session/harness/event are nonempty strings up to 256
UTF-8 bytes without control characters. Ciphertext must be canonical standard
base64 and decode to at most 512 KiB. The Worker parses age v1 recipient
stanzas, canonical header MAC framing and a minimum nonce/tag body; password
recipients and incomplete/prefix-only headers are rejected. This
[age format guard](https://c2sp.org/age@v1.1.0) cannot authenticate the header
MAC or encrypted payload, or prove that arbitrary client content is secret.

Successful upload returns `200 {"raw_ref":"..."}`. Retrying an identical
normalized envelope succeeds; different content/context for an existing ref
returns `409`. Fetch returns the exact envelope or `404` within the authorized
tenant. Another tenant cannot select that object's namespace. No plaintext raw
field or decryption endpoint is supported.

Enrolled clients encrypt/authenticate an inner payload containing context,
fields and owning span IDs. Readers verify those bindings locally. Recipient
manifests are signed, but raw objects carry no writer signature or complete
replay/provenance protocol. See
[raw enrollment](../guides/encrypted-raw-values.md).

## Deploy and operate

Follow the [quick start](../getting-started/quickstart.md) for Cloudflare setup,
or [local setup](../getting-started/local-collector.md) for a localhost Worker and local D1.
The `DB` binding and database ID live in `worker/wrangler.toml`; `GENTLY_HOSTS`
is supplied separately. Avoid committing tokens to source or configuration.

For development from the checkout:

```sh
cd worker
npm ci
npm test
npm run typecheck
```

There is no built-in deletion, retention scheduler, token provisioning endpoint,
or query pagination. Database maintenance and credential rotation are operator
responsibilities. The [configuration guide](../getting-started/configuration.md)
describes exporter limits independently of database retention.

## Source

[Routing and auth](https://github.com/JamieAP/gently/blob/main/worker/src/index.ts),
[OTLP flattening](https://github.com/JamieAP/gently/blob/main/worker/src/otlp.ts), and
[database operations](https://github.com/JamieAP/gently/blob/main/worker/src/d1.ts) define this API.
