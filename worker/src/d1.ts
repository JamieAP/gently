import type { Row } from "./otlp.js";
import { ClientError } from "./http.js";

export interface Env {
  DB: D1Database;
  GENTLY_HOSTS: string;
}

// Deliberately project the public metadata fields. New storage columns must not
// silently become query response fields.
const SPAN_COLUMNS = [
  "span_id", "trace_id", "parent_span_id", "name", "kind", "start_unix_nano", "end_unix_nano",
  "status", "session_id", "harness", "tool_name", "tool_use_id", "attrs_json", "resource_json", "ingested_unix_nano",
];

const DEFAULT_LIMIT = 50;
const MAX_LIMIT = 1000;
const START_ASC = "start_asc";
const START_DESC = "start_desc";
const LAST_ACTIVITY = "last_activity";
const LAST_ACTIVITY_ASC = "last_activity_asc";
const LAST_ACTIVITY_DESC = "last_activity_desc";

function clampLimit(raw: string | null, def = DEFAULT_LIMIT): number {
  if (!raw) return def;
  const n = parseInt(raw, 10);
  if (isNaN(n) || n < 1) return def;
  return Math.min(n, MAX_LIMIT);
}

function startOrder(raw: string | null | undefined, def = START_DESC): string {
  return raw === START_ASC ? "ASC" : def === START_ASC ? "ASC" : "DESC";
}

function traceOrder(raw: string | null | undefined): { column: string; direction: string } {
  switch (raw) {
    case START_ASC:
      return { column: "start", direction: "ASC" };
    case LAST_ACTIVITY:
    case LAST_ACTIVITY_DESC:
      return { column: "last_activity", direction: "DESC" };
    case LAST_ACTIVITY_ASC:
      return { column: "last_activity", direction: "ASC" };
    default:
      return { column: "start", direction: "DESC" };
  }
}

function intFilter(raw: string | null | undefined): number | null {
  if (raw === null || raw === undefined || raw === "") return null;
  const n = parseInt(raw, 10);
  return Number.isFinite(n) ? n : null;
}

export async function insertSpans(env: Env, tenantId: string, deviceId: string, rows: Row[]): Promise<void> {
  if (rows.length === 0) return;
  const expectedTraces = new Map<string, string>();
  for (const row of rows) {
    const previousTrace = expectedTraces.get(row.span_id);
    if (previousTrace !== undefined && previousTrace !== row.trace_id) {
      throw new ClientError(409, "Span belongs to another trace");
    }
    expectedTraces.set(row.span_id, row.trace_id);
  }
  // Repeated lifecycle reports share an ID. Read ownership once per unique ID,
  // using one tenant binding plus at most 99 IDs per D1 statement.
  const ids = [...expectedTraces.keys()];
  const ownershipStatements = () => {
    const statements: D1PreparedStatement[] = [];
    for (let offset = 0; offset < ids.length; offset += 99) {
      const chunk = ids.slice(offset, offset + 99);
      statements.push(env.DB.prepare(
        `SELECT span_id, source_device_id, trace_id FROM spans WHERE tenant_id = ? AND span_id IN (${chunk.map(() => "?").join(",")})`,
      ).bind(tenantId, ...chunk));
    }
    return statements;
  };
  type Owner = { span_id: string; source_device_id: string; trace_id: string };
  const owners = await env.DB.batch<Owner>(ownershipStatements());
  const conflicts = (owner: Owner) => owner.source_device_id !== deviceId || owner.trace_id !== expectedTraces.get(owner.span_id);
  if (owners.some(result => result.results.some(conflicts))) {
    throw new ClientError(409, "Span belongs to another capture device or trace");
  }

  // Idempotent-monotonic upsert. The hook layer is a distributed, retrying,
  // multi-process emitter with no delivery-order guarantee: the same span_id is
  // reported provisionally on open, again on close, and (for the session root)
  // again on every resume. `INSERT OR REPLACE` let the last-delivered report win
  // — so a retried provisional could revert a finalized span, and a resume could
  // shove the session root's start forward past its own history.
  // Lifecycle extents keep earliest start/latest end. Tool completion outranks
  // a provisional open, and a runtime-reported duration outranks inferred
  // hook-pair timing: keep that report's start AND end together. This prevents
  // pre-hook overhead from expanding a precise tool duration on merge.
  // Equal-quality reports pick content by latest end; ties favour the newcomer.
  // Replays and out-of-order delivery otherwise converge. nanos are CAST to
  // INTEGER (< 2^63) only for comparison; the stored value stays the TEXT we got.
  // Only fixed SQL aliases and attribute literals below may reach this helper;
  // never pass request-derived strings here. User filters are bound separately.
  const hasAttr = (prefix: string, key: string, value?: string) =>
    `EXISTS (SELECT 1 FROM json_each(${prefix}attrs_json) a WHERE
      json_extract(a.value, '$.key') = '${key}'${value === undefined ? "" :
      ` AND json_extract(a.value, '$.value.stringValue') = '${value}'`})`;
  const quality = (prefix: string) => `(CASE
    WHEN ${prefix}tool_name IS NULL THEN 0
    WHEN ${hasAttr(prefix, "gently.tool_state", "closed")}
      OR ${hasAttr(prefix, "gently.event", "PostToolUse")}
      OR ${hasAttr(prefix, "gently.event", "PostToolUseFailure")}
    THEN CASE WHEN ${hasAttr(prefix, "gently.tool_duration_ms")} THEN 2 ELSE 1 END
    ELSE 0 END)`;
  const incomingQuality = "(SELECT incoming_quality FROM decision)";
  const storedQuality = "(SELECT stored_quality FROM decision)";
  const newer = "(SELECT prefer_incoming FROM decision)";
  const pick = (col: string) => `${col} = CASE WHEN ${newer} THEN excluded.${col} ELSE ${col} END`;
  // Materialize the previous report and merge decision once, before mutation.
  // Re-expanding JSON predicates for every column makes batch SQL unnecessarily
  // large and repeatedly scans the same attribute array.
  const sql = `WITH previous AS MATERIALIZED (
         SELECT ${quality("spans.")} AS quality,
           COALESCE(end_unix_nano, start_unix_nano) AS endpoint
         FROM spans WHERE tenant_id = ?1 AND span_id = ?2
       ), decision AS MATERIALIZED (
         SELECT ?18 AS incoming_quality, COALESCE(MAX(quality), 0) AS stored_quality,
           (?18 > COALESCE(MAX(quality), 0) OR
             (?18 = COALESCE(MAX(quality), 0) AND
               CAST(COALESCE(?9, ?8) AS INTEGER) >= CAST(MAX(endpoint) AS INTEGER))) AS prefer_incoming
         FROM previous
       )
       INSERT INTO spans
        (tenant_id, span_id, source_device_id, trace_id, parent_span_id, name, kind,
         start_unix_nano, end_unix_nano, status,
         session_id, harness, tool_name, tool_use_id,
         attrs_json, resource_json, ingested_unix_nano)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
       ON CONFLICT(tenant_id, span_id) DO UPDATE SET
         start_unix_nano = CASE
           WHEN ${incomingQuality} = 2 OR ${storedQuality} = 2
           THEN CASE WHEN ${newer} THEN excluded.start_unix_nano ELSE spans.start_unix_nano END
           WHEN CAST(excluded.start_unix_nano AS INTEGER) < CAST(start_unix_nano AS INTEGER)
           THEN excluded.start_unix_nano ELSE start_unix_nano END,
         end_unix_nano = CASE
           WHEN ${incomingQuality} != ${storedQuality} OR ${incomingQuality} = 2
           THEN CASE WHEN ${newer} THEN excluded.end_unix_nano ELSE spans.end_unix_nano END
           WHEN CAST(COALESCE(excluded.end_unix_nano, excluded.start_unix_nano) AS INTEGER)
              > CAST(COALESCE(end_unix_nano, start_unix_nano) AS INTEGER)
           THEN excluded.end_unix_nano ELSE end_unix_nano END,
         ${pick("status")},
         ${pick("name")},
         ${pick("kind")},
         ${pick("parent_span_id")},
         ${pick("session_id")},
         ${pick("harness")},
         ${pick("tool_name")},
         ${pick("tool_use_id")},
         ${pick("attrs_json")},
         ${pick("resource_json")},
         ${pick("ingested_unix_nano")}
         WHERE source_device_id = excluded.source_device_id AND trace_id = excluded.trace_id`;
  const stmts = rows.map((r) =>
    env.DB.prepare(sql).bind(
      tenantId,
      r.span_id,
      deviceId,
      r.trace_id,
      r.parent_span_id,
      r.name,
      r.kind,
      r.start_unix_nano,
      r.end_unix_nano,
      r.status,
      r.session_id,
      r.harness,
      r.tool_name,
      r.tool_use_id,
      r.attrs_json,
      r.resource_json,
      r.ingested_unix_nano,
      toolReportQuality(r),
    ),
  );

  await env.DB.batch(stmts);
  // A competing device may claim an absent ID after preflight. The guarded
  // upsert cannot overwrite it, and verification reports the conflict safely.
  const storedOwners = await env.DB.batch<Owner>(ownershipStatements());
  const stored = storedOwners.flatMap(result => result.results);
  if (stored.length !== ids.length || stored.some(conflicts)) {
    throw new ClientError(409, "Span belongs to another capture device or trace");
  }
}

function toolReportQuality(row: Row): number {
  if (row.tool_name === null) return 0;
  const attrs = JSON.parse(row.attrs_json) as Array<{key: string; value?: {stringValue?: string}}>;
  const has = (key: string, value: string) => attrs.some(a => a.key === key && a.value?.stringValue === value);
  const closed = has("gently.tool_state", "closed") ||
    has("gently.event", "PostToolUse") || has("gently.event", "PostToolUseFailure");
  if (!closed) return 0;
  return attrs.some(a => a.key === "gently.tool_duration_ms") ? 2 : 1;
}

export interface TraceSummary {
  trace_id: string;
  session_id: string | null;
  harness: string | null;
  start: string;
  last_activity: string;
  span_count: number;
  error_count: number;
}

// Older producers lifted tool_name onto permission markers. Keep those rows
// visible in traces/searches, but do not treat observations as tool executions.
const TOOL_EXECUTION = `tool_name IS NOT NULL AND NOT EXISTS (
  SELECT 1 FROM json_each(attrs_json) a
  WHERE json_extract(a.value, '$.key') = 'gently.event'
    AND json_extract(a.value, '$.value.stringValue') IN ('PermissionRequest', 'PermissionDenied')
)`;

export async function traces(
  env: Env,
  tenantId: string,
  params: {
    limit?: string | null;
    harness?: string | null;
    session_id?: string | null;
    since?: string | null;
    until?: string | null;
    order?: string | null;
  },
): Promise<TraceSummary[]> {
  const limit = clampLimit(params.limit ?? null);
  const conditions: string[] = ["tenant_id = ?"];
  const bindings: (string | number)[] = [tenantId];

  if (params.since) {
    conditions.push("start_unix_nano >= ?");
    bindings.push(params.since);
  }
  if (params.harness) {
    conditions.push("harness = ?");
    bindings.push(params.harness);
  }
  if (params.session_id) {
    conditions.push("session_id = ?");
    bindings.push(params.session_id);
  }
  if (params.until) {
    conditions.push("start_unix_nano <= ?");
    bindings.push(params.until);
  }

  const where = conditions.length > 0 ? `WHERE ${conditions.join(" AND ")}` : "";
  const order = traceOrder(params.order);

  const stmt = env.DB.prepare(
    `SELECT trace_id, session_id, harness,
            MIN(start_unix_nano) AS start,
            MAX(COALESCE(end_unix_nano, start_unix_nano)) AS last_activity,
            COUNT(*) AS span_count,
            -- error_count is failed TOOL calls only. Turn spans carry status=2 on
            -- StopFailure (an aborted/interrupted turn - common in long autonomous
            -- loops) and the session root can too; counting those would drown the
            -- tool-failure signal. The status stays on those spans for rendering;
            -- it just doesn't count here. tool_name IS NOT NULL ⇒ it's a tool span.
            SUM(status = 2 AND (${TOOL_EXECUTION})) AS error_count
     FROM spans
     ${where}
     GROUP BY trace_id
     ORDER BY ${order.column} ${order.direction}
     LIMIT ?`,
  ).bind(...bindings, limit);

  const result = await stmt.all<TraceSummary>();
  return result.results;
}

export interface SpanRow {
  span_id: string;
  trace_id: string;
  parent_span_id: string | null;
  name: string;
  kind: number;
  start_unix_nano: string;
  end_unix_nano: string | null;
  status: number;
  session_id: string | null;
  harness: string | null;
  tool_name: string | null;
  tool_use_id: string | null;
  attrs_json: string | null;
  resource_json: string | null;
  ingested_unix_nano: string;
  // Derived display bounds from the observations selected by `trace()`.
  // Their aggregation is not proof of complete capture or actual completion.
  effective_start_unix_nano: string;
  effective_end_unix_nano: string;
}


export interface TracePage { rows: SpanRow[]; next_cursor: string | null; complete: boolean }

// Page budgets, in response bytes. A page holds up to `rows` rows within `bytes`;
// a row too large to share a page comes back alone, up to `loneRowBytes`. Ingest
// accepts at most 1 MiB per request, so every stored row fits on some page.
interface Budget { rows: number; bytes: number; loneRowBytes: number }
const PAGE_ROWS = 100;
// Room for `{"rows":[…],"next_cursor":"<at most 4096 chars>","complete":false}`.
const PAGE_ENVELOPE = 4096 + 64;
const pageBudget = (rows: number): Budget => ({
  rows, bytes: 2 * 1024 * 1024 - PAGE_ENVELOPE, loneRowBytes: 8 * 1024 * 1024 - PAGE_ENVELOPE,
});
// Unpaged callers predate pagination and accept responses of up to 8 MiB.
const LEGACY: Budget = { rows: 10_000, bytes: 8 * 1024 * 1024, loneRowBytes: 8 * 1024 * 1024 };

// Rows are read in position order, (length(start), start, span_id), folded into
// one TEXT key so that an expression index serves both the keyset seek and the
// ORDER BY. For canonical unsigned decimal nanos this is numeric order across
// the whole u64 range; other stored strings still get one deterministic place,
// so no row is skipped. These must stay structurally identical to the
// idx_spans_trace_position and idx_spans_trace_end expressions in schema.sql.
const position = (start: string, span: string) =>
  `(printf('%09d', length(${start})) || ':' || ${start} || ':' || ${span})`;
const POSITION = position("start_unix_nano", "span_id");
const END = "COALESCE(end_unix_nano, start_unix_nano)";
const END_POSITION = `(printf('%09d', length(${END})) || ':' || ${END})`;
// Stored bytes never exceed a row's JSON size, so admitting rows by stored bytes
// cannot exclude a row that fits; the exact JSON size is checked afterwards.
const STORED_BYTES = SPAN_COLUMNS.map(column => `length(CAST(COALESCE(${column}, '') AS BLOB))`).join(" + ");

// Derived display bounds: parentless records use the trace's first start and
// last end in position order (exact for canonical u64 text); other records use
// own/direct-child minimum start and retain non-provisional ends, otherwise
// using direct-child maximum end or their own start. A page reads only its own
// rows and their direct children; the trace-wide bounds are two index seeks.
// Parentless rows carry those bounds, so admission counts them as row bytes.
// These observations do not prove capture completeness or actual completion.
// Raw bounds remain unchanged; TEXT avoids JS integer rounding.
export function tracePageSql(afterCursor: boolean): string {
  return `WITH agg AS MATERIALIZED (
       SELECT (SELECT start_unix_nano FROM spans WHERE tenant_id = ?1 AND trace_id = ?2
               ORDER BY ${POSITION} LIMIT 1) AS trace_min,
              (SELECT ${END} FROM spans WHERE tenant_id = ?1 AND trace_id = ?2
               ORDER BY ${END_POSITION} DESC LIMIT 1) AS trace_max
     ), candidate AS MATERIALIZED (
       SELECT span_id, ${POSITION} AS position,
              ${STORED_BYTES} + CASE WHEN parent_span_id IS NULL
                THEN length(CAST(agg.trace_min AS BLOB)) + length(CAST(agg.trace_max AS BLOB)) ELSE 0 END AS stored_bytes
       FROM spans CROSS JOIN agg WHERE tenant_id = ?1 AND trace_id = ?2
         ${afterCursor ? `AND ${POSITION} > ${position("?3", "?4")}` : ""}
       ORDER BY ${POSITION} LIMIT ?5 + 1
     ), ranked AS MATERIALIZED (
       SELECT span_id, position, ROW_NUMBER() OVER w AS rn, SUM(stored_bytes) OVER w AS bytes
       FROM candidate WINDOW w AS (ORDER BY position)
     ), page AS MATERIALIZED (
       SELECT span_id, position FROM ranked WHERE rn <= ?5 AND (rn = 1 OR bytes <= ?6)
     ), child_bounds AS MATERIALIZED (
       SELECT parent_span_id AS pid,
              MAX(CAST(${END} AS INTEGER)) AS cmax,
              MIN(CAST(start_unix_nano AS INTEGER)) AS cmin
       FROM spans
       WHERE tenant_id = ?1 AND trace_id = ?2 AND parent_span_id IN (SELECT span_id FROM page)
       GROUP BY parent_span_id
     )
     SELECT ${SPAN_COLUMNS.map(column => `s.${column}`).join(", ")},
            CAST(CASE
              WHEN s.parent_span_id IS NULL THEN agg.trace_min
              ELSE MIN(CAST(s.start_unix_nano AS INTEGER),
                       COALESCE(cb.cmin, CAST(s.start_unix_nano AS INTEGER)))
            END AS TEXT) AS effective_start_unix_nano,
            CAST(CASE
              -- Parentless display bounds use the whole recorded trace, even
              -- when the stored endpoint is non-provisional. A resumed root's
              -- latest SessionStart can extend that stored endpoint without a
              -- closing hook. The aggregate is an observation, not proof of
              -- actual completion; this case precedes the finalized check.
              WHEN s.parent_span_id IS NULL
                THEN agg.trace_max
              WHEN s.end_unix_nano IS NOT NULL AND s.end_unix_nano <> s.start_unix_nano
                THEN CAST(s.end_unix_nano AS INTEGER)
              ELSE COALESCE(cb.cmax, CAST(s.start_unix_nano AS INTEGER))
            END AS TEXT) AS effective_end_unix_nano,
            (SELECT COUNT(*) FROM candidate) AS candidates
     FROM page JOIN spans s ON s.tenant_id = ?1 AND s.span_id = page.span_id
     CROSS JOIN agg LEFT JOIN child_bounds cb ON cb.pid = s.span_id
     ORDER BY page.position`;
}

// A cursor names the last row returned and the trace generation the read began
// at. Its fields are positions, not credentials: every query stays tenant-scoped.
interface TraceCursor { v: 1; tenant: string; trace: string; generation: string; start: string; span: string }

function decodeCursor(raw: string | null, tenant: string, trace: string): TraceCursor | null {
  if (!raw) return null;
  try {
    if (raw.length > 4096 || !/^[A-Za-z0-9_-]+$/.test(raw)) throw new Error();
    const cursor = JSON.parse(new TextDecoder("utf-8", {fatal:true,ignoreBOM:false}).decode(Uint8Array.from(atob(raw.replace(/-/g, "+").replace(/_/g, "/")), c => c.charCodeAt(0))));
    if (cursor?.v !== 1 || cursor.tenant !== tenant || cursor.trace !== trace
        || typeof cursor.generation !== "string" || !/^[0-9]{1,19}$/.test(cursor.generation)
        || typeof cursor.start !== "string" || cursor.start.length > 128
        || typeof cursor.span !== "string" || cursor.span.length === 0 || cursor.span.length > 256) throw new Error();
    // Rebuild rather than spread: unknown fields never reach the next cursor.
    return { v: 1, tenant, trace, generation: cursor.generation, start: cursor.start, span: cursor.span };
  } catch { throw new ClientError(400, "Invalid trace cursor"); }
}
function encodeCursor(cursor: TraceCursor): string {
  if (!cursor.span || cursor.span.length > 256 || cursor.start.length > 128)
    throw new ClientError(413, "Stored trace position exceeds the continuation budget");
  const encoded = btoa(String.fromCharCode(...new TextEncoder().encode(JSON.stringify(cursor)))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  if (encoded.length > 4096) throw new ClientError(413, "Trace cursor exceeds the bounded continuation budget");
  return encoded;
}

function requireTraceId(traceId: string): void {
  if (!traceId || traceId.length > 256) throw new ClientError(400, "Invalid trace_id");
}

// Legacy callers receive a complete bounded trace or an explicit error; never a
// silently truncated array. New clients request page=1 and follow next_cursor.
export async function trace(env: Env, tenantId: string, traceId: string): Promise<SpanRow[]> {
  requireTraceId(traceId);
  const slice = await traceSlice(env, tenantId, traceId, null, LEGACY);
  if (slice.more) throw new ClientError(413, "Trace requires cursor pagination (page=1)");
  return slice.rows;
}

export async function tracePage(env: Env, tenantId: string, traceId: string, cursorRaw: string | null, limitRaw: string | null): Promise<TracePage> {
  requireTraceId(traceId);
  const cursor = decodeCursor(cursorRaw, tenantId, traceId);
  if (limitRaw !== null && !/^(?:[1-9]|[1-9][0-9]|100)$/.test(limitRaw)) throw new ClientError(400, "Invalid page limit");
  const slice = await traceSlice(env, tenantId, traceId, cursor, pageBudget(limitRaw === null ? PAGE_ROWS : Number(limitRaw)));
  const last = slice.rows.at(-1);
  const next = slice.more && last
    ? encodeCursor({ v: 1, tenant: tenantId, trace: traceId, generation: slice.generation, start: last.start_unix_nano, span: last.span_id })
    : null;
  return { rows: slice.rows, next_cursor: next, complete: next === null };
}

interface TraceSlice { rows: SpanRow[]; more: boolean; generation: string }

// One page read. The generation and the page come from one D1 batch, which is
// one transaction, so they describe the same state of the trace.
async function traceSlice(env: Env, tenantId: string, traceId: string, cursor: TraceCursor | null, budget: Budget): Promise<TraceSlice> {
  const state = env.DB.prepare(
    `SELECT CAST(COALESCE((SELECT generation FROM trace_generations WHERE tenant_id = ?1 AND trace_id = ?2), 0) AS TEXT) AS generation,
            (SELECT start_unix_nano FROM spans WHERE tenant_id = ?1 AND trace_id = ?2 AND span_id = ?3) AS cursor_start`,
  ).bind(tenantId, traceId, cursor?.span ?? null);
  const page = env.DB.prepare(tracePageSql(cursor !== null))
    .bind(tenantId, traceId, cursor?.start ?? null, cursor?.span ?? null, budget.rows, budget.bytes);
  const [stateResult, pageResult] = await env.DB.batch<Record<string, unknown>>([state, page]);
  const { generation, cursor_start } = stateResult.results[0] as { generation: string; cursor_start: string | null };
  if (cursor && cursor.generation !== generation) {
    throw new ClientError(409, "Trace changed during pagination; repeat the query");
  }
  // An unchanged generation means the cursor's row has not moved, so a
  // different stored start can only be a forged position.
  if (cursor && cursor.start !== cursor_start) throw new ClientError(400, "Invalid trace cursor");
  const fetched = pageResult.results as unknown as (SpanRow & { candidates: number })[];
  const rows = fitRows(fetched.map(({ candidates: _, ...row }) => row), budget);
  return { rows, more: (fetched[0]?.candidates ?? 0) > rows.length, generation };
}

// The longest prefix whose JSON array fits the budget, or a lone oversized row.
function fitRows(rows: SpanRow[], budget: Budget): SpanRow[] {
  const encoder = new TextEncoder();
  let bytes = 2;
  for (const [index, row] of rows.entries()) {
    bytes += encoder.encode(JSON.stringify(row)).byteLength + (index > 0 ? 1 : 0);
    if (bytes <= budget.bytes) continue;
    if (index > 0) return rows.slice(0, index);
    if (bytes > budget.loneRowBytes) throw new ClientError(413, "A trace row exceeds the bounded page budget");
    return rows.slice(0, 1);
  }
  return rows;
}

export async function spans(
  env: Env,
  tenantId: string,
  params: {
    trace_id?: string | null;
    session_id?: string | null;
    harness?: string | null;
    tool_name?: string | null;
    name?: string | null;
    status?: string | null;
    kind?: string | null;
    since?: string | null;
    until?: string | null;
    limit?: string | null;
    order?: string | null;
  },
): Promise<SpanRow[]> {
  const limit = clampLimit(params.limit ?? null);
  const conditions: string[] = ["tenant_id = ?"];
  const bindings: (string | number)[] = [tenantId];

  if (params.trace_id) {
    conditions.push("trace_id = ?");
    bindings.push(params.trace_id);
  }
  if (params.session_id) {
    conditions.push("session_id = ?");
    bindings.push(params.session_id);
  }
  if (params.harness) {
    conditions.push("harness = ?");
    bindings.push(params.harness);
  }
  if (params.tool_name) {
    conditions.push("tool_name = ?");
    bindings.push(params.tool_name);
  }
  if (params.name) {
    conditions.push("name = ?");
    bindings.push(params.name);
  }
  const status = intFilter(params.status);
  if (status !== null) {
    conditions.push("status = ?");
    bindings.push(status);
  }
  const kind = intFilter(params.kind);
  if (kind !== null) {
    conditions.push("kind = ?");
    bindings.push(kind);
  }
  if (params.since) {
    conditions.push("start_unix_nano >= ?");
    bindings.push(params.since);
  }
  if (params.until) {
    conditions.push("start_unix_nano <= ?");
    bindings.push(params.until);
  }

  const where = conditions.length > 0 ? `WHERE ${conditions.join(" AND ")}` : "";
  const direction = startOrder(params.order, START_DESC);

  const result = await env.DB.prepare(
    `SELECT ${SPAN_COLUMNS.join(", ")} FROM spans ${where} ORDER BY start_unix_nano ${direction} LIMIT ?`,
  )
    .bind(...bindings, limit)
    .all<SpanRow>();
  return result.results;
}

export interface ToolStat {
  tool_name: string;
  span_count: number;
  error_count: number;
  avg_duration_ms: number | null;
  // p50/p95 not available in SQLite; see follow-up for approximation
}

export async function stats(env: Env, tenantId: string): Promise<ToolStat[]> {
  const result = await env.DB.prepare(
    `SELECT
       tool_name,
       COUNT(*) AS span_count,
       SUM(status = 2) AS error_count,
       AVG(
         CASE
           WHEN end_unix_nano IS NOT NULL AND NOT (EXISTS (
             SELECT 1 FROM json_each(attrs_json) a
             WHERE json_extract(a.value, '$.key') = 'gently.tool_state'
               AND json_extract(a.value, '$.value.stringValue') = 'open'
           ) AND NOT EXISTS (
             SELECT 1 FROM json_each(attrs_json) a
             WHERE (json_extract(a.value, '$.key') = 'gently.tool_state'
               AND json_extract(a.value, '$.value.stringValue') = 'closed')
               OR (json_extract(a.value, '$.key') = 'gently.event'
                 AND json_extract(a.value, '$.value.stringValue') IN ('PostToolUse', 'PostToolUseFailure'))
           ))
           THEN (CAST(end_unix_nano AS REAL) - CAST(start_unix_nano AS REAL)) / 1e6
           ELSE NULL
         END
       ) AS avg_duration_ms
     FROM spans
     WHERE tenant_id = ? AND (${TOOL_EXECUTION})
     GROUP BY tool_name
     ORDER BY span_count DESC`,
  ).bind(tenantId).all<ToolStat>();
  return result.results;
}
