# Collector and Worker

The included collector is a TypeScript Cloudflare Worker backed by D1. It can run
locally through Wrangler or be deployed to Cloudflare. CLI and MCP queries read
this collector, rather than the local outbox.

## Authentication

Every route requires `Authorization: Bearer <token>` matching the Worker's
`GENTLY_TOKEN`. A missing, empty, or incorrect token returns `401`. The shared
token grants access to all stored traces; there is no per-user or tenant access
boundary and no automatic database retention policy.

The implementation returns JSON with `Cache-Control: no-store` and
`X-Content-Type-Options: nosniff`. These headers do not remove previously stored
span data. See [security and privacy](../concepts/security-and-privacy.md).

## Endpoints

| Method and path | Purpose |
| --- | --- |
| `POST /v1/traces` | Flatten an OTLP/JSON request and upsert its spans |
| `GET /v1/query?op=traces` | Trace/session summaries |
| `GET /v1/query?op=trace&trace_id=TRACE_ID` | All rows for one trace, raw start ascending, with effective bounds |
| `GET /v1/query?op=spans` | Limited, filtered span search |
| `GET /v1/query?op=stats` | Unfiltered per-tool rollups |
| `GET /v1/whoami` | Protocol probe for this HTTP request |

Successful ingest returns:

```json
{"partialSuccess":{},"httpProtocol":"HTTP/3"}
```

`httpProtocol` comes from request metadata and can be `"unknown"` in environments
without it. `/v1/whoami` returns only that field. It does not identify a user or
session and is unrelated to `gently whoami --pane`.

Unsupported methods, paths, and query operations return `404`. JSON parsing,
flattening, or database exceptions return a generic `500`. The ingest handler
does not provide detailed per-span validation or rejection counts; an empty
span envelope can succeed without inserting rows.

### Example query

With the collector URL configured and a token supplied to the process:

```sh
gently traces --harness codex --limit 5 --order last_activity --json
```

The CLI reads `GENTLY_TOKEN` from its environment and adds the bearer header.
For a custom client, the corresponding request is:

```http
GET /v1/query?op=traces&harness=codex&limit=5&order=last_activity HTTP/1.1
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
uses `span_id` as the primary key and preserves the earliest reported start and
latest reported end-or-start. Status, name, parent, and attribute fields come
from the report whose end-or-start is latest. Equal ending timestamps favor
the incoming report, so conflicting metadata at a tie depends on delivery order.
Stored trace IDs are not updated on conflict; span IDs must therefore identify
the same logical span consistently.

Timestamp strings preserve the received representation, but merge comparisons
and effective bounds cast them to SQLite signed integers. The implementation
assumes values within that range; it does not validate every incoming OTLP
field or support arbitrary unsigned-64 timestamp comparisons correctly.

The ingest log records the request protocol and span count, not payload content.
The response's empty `partialSuccess` object does not report rejected-span counts.

## D1 schema (`worker/schema.sql`)

The `spans` table stores one row per `span_id`. It indexes trace, session, raw
start, tool, and several compound filter/start keys. Effective bounds are query
results, not columns. See [schema.sql](https://github.com/JamieAP/gently/blob/main/worker/schema.sql) for the exact
DDL and [d1.ts](https://github.com/JamieAP/gently/blob/main/worker/src/d1.ts) for query and merge logic.

## Deploy and operate

Follow the [quick start](../getting-started/quickstart.md) for Cloudflare setup,
or [local setup](../getting-started/local-collector.md) for a localhost Worker and local D1.
The `DB` binding and database ID live in `worker/wrangler.toml`; `GENTLY_TOKEN`
is supplied separately. Avoid committing tokens to source or configuration.

For development from the checkout:

```sh
cd worker
npm ci
npm test
```

There is no built-in deletion, retention scheduler, token provisioning endpoint,
or query pagination. Database maintenance and credential rotation are operator
responsibilities. The [configuration guide](../getting-started/configuration.md)
describes exporter limits independently of database retention.

## Source

[Routing and auth](https://github.com/JamieAP/gently/blob/main/worker/src/index.ts),
[OTLP flattening](https://github.com/JamieAP/gently/blob/main/worker/src/otlp.ts), and
[database operations](https://github.com/JamieAP/gently/blob/main/worker/src/d1.ts) define this API.
