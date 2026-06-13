# Collector & Worker

The collector is `gently-collector`, a TypeScript Cloudflare Worker backed by a
D1 database (`worker/`). It is the durable store of record and the only query
source.

## Endpoints

All require `Authorization: Bearer <GENTLY_TOKEN>` → 401 otherwise.

| Method · path | Purpose |
|---|---|
| `POST /v1/traces` | OTLP/JSON ingest. Flattens spans, upserts into D1 with an **idempotent-monotonic** merge on `span_id` (envelope bounds: `MIN(start)`/`MAX(end)`, content from the latest-ending report, with incoming metadata winning equal-end ties). Echoes the negotiated `httpProtocol`. |
| `GET /v1/query?op=traces` | traces/sessions aggregated by `trace_id`: `trace_id`, `session_id`, `harness`, `start`, `last_activity`, `span_count`, `error_count`. Filters: `session_id`, `harness`, `since`, `until`, `limit`, `order` (`start_desc`, `start_asc`, `last_activity`, `last_activity_desc`, `last_activity_asc`). |
| `GET /v1/query?op=trace&trace_id=` | all spans for a trace, ordered by start; each row also carries derived `effective_start_unix_nano` / `effective_end_unix_nano` (observed trace-wide or direct-child aggregate bounds, with existing non-provisional non-root ends retained; not proof of completeness - see [Trace model](../concepts/trace-model.md#effective-bounds)). |
| `GET /v1/query?op=spans&…` | filtered spans (`trace_id`, `session_id`, `harness`, `tool_name`, `name`, `status`, `kind`, `since`, `until`, `limit`, `order`). |
| `GET /v1/query?op=stats` | per-tool counts, error counts, average duration. |
| `GET /v1/whoami` | returns the negotiated protocol (e.g. `{"httpProtocol":"HTTP/3"}`) - used to confirm QUIC on the wire. |

## D1 schema (`worker/schema.sql`)

```sql
CREATE TABLE spans (
  span_id TEXT PRIMARY KEY,
  trace_id TEXT NOT NULL,
  parent_span_id TEXT,
  name TEXT NOT NULL,
  kind INTEGER NOT NULL,
  start_unix_nano TEXT NOT NULL,   -- uint64 as string (precision-safe)
  end_unix_nano TEXT,
  status INTEGER NOT NULL DEFAULT 0,
  session_id TEXT, harness TEXT,
  tool_name TEXT, tool_use_id TEXT,
  attrs_json TEXT, resource_json TEXT,
  ingested_unix_nano TEXT NOT NULL
);
-- indexes on trace_id/session_id/harness/tool_name/name/status/kind + start_unix_nano
```

Trace-scoped attributes (`session_id`, `harness`) are lifted from the OTLP
resource; `tool_name`/`tool_use_id` from the span; the rest stays in
`attrs_json` / `resource_json`.

## Deploy & operate

```bash
cd worker
npm install
wrangler d1 create gently                       # → database_id (into wrangler.toml)
wrangler d1 execute gently --remote --file schema.sql
wrangler secret put GENTLY_TOKEN
wrangler deploy

npm test                                         # vitest + Miniflare (local D1)
wrangler tail gently-collector --format json     # live logs (shows ingest httpProtocol)
wrangler d1 execute gently --command "SELECT count(*) FROM spans"
```

Local development: `echo 'GENTLY_TOKEN=dev' > .dev.vars`, apply the schema with
`--local`, then `wrangler dev` serves on `http://127.0.0.1:8787`.
