import type { Row } from "./otlp.js";

export interface Env {
  DB: D1Database;
  GENTLY_TOKEN: string;
}

const DEFAULT_LIMIT = 50;
const MAX_LIMIT = 1000;
const START_ASC = "start_asc";
const START_DESC = "start_desc";

function clampLimit(raw: string | null, def = DEFAULT_LIMIT): number {
  if (!raw) return def;
  const n = parseInt(raw, 10);
  if (isNaN(n) || n < 1) return def;
  return Math.min(n, MAX_LIMIT);
}

function startOrder(raw: string | null | undefined, def = START_DESC): string {
  return raw === START_ASC ? "ASC" : def === START_ASC ? "ASC" : "DESC";
}

function intFilter(raw: string | null | undefined): number | null {
  if (raw === null || raw === undefined || raw === "") return null;
  const n = parseInt(raw, 10);
  return Number.isFinite(n) ? n : null;
}

export async function insertSpans(env: Env, rows: Row[]): Promise<void> {
  if (rows.length === 0) return;

  const stmts = rows.map((r) =>
    env.DB.prepare(
      `INSERT OR REPLACE INTO spans
        (span_id, trace_id, parent_span_id, name, kind,
         start_unix_nano, end_unix_nano, status,
         session_id, harness, tool_name, tool_use_id,
         attrs_json, resource_json, ingested_unix_nano)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    ).bind(
      r.span_id,
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
    ),
  );

  await env.DB.batch(stmts);
}

export interface TraceSummary {
  trace_id: string;
  session_id: string | null;
  harness: string | null;
  start: string;
  span_count: number;
  error_count: number;
}

export async function traces(
  env: Env,
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
  const conditions: string[] = [];
  const bindings: (string | number)[] = [];

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
  const direction = startOrder(params.order, START_DESC);

  const stmt = env.DB.prepare(
    `SELECT trace_id, session_id, harness,
            MIN(start_unix_nano) AS start,
            COUNT(*) AS span_count,
            SUM(status = 2) AS error_count
     FROM spans
     ${where}
     GROUP BY trace_id
     ORDER BY start ${direction}
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
}

export async function trace(env: Env, trace_id: string): Promise<SpanRow[]> {
  const result = await env.DB.prepare(
    `SELECT * FROM spans WHERE trace_id = ? ORDER BY start_unix_nano ASC`,
  )
    .bind(trace_id)
    .all<SpanRow>();
  return result.results;
}

export async function spans(
  env: Env,
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
  const conditions: string[] = [];
  const bindings: (string | number)[] = [];

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
    `SELECT * FROM spans ${where} ORDER BY start_unix_nano ${direction} LIMIT ?`,
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

export async function stats(env: Env): Promise<ToolStat[]> {
  const result = await env.DB.prepare(
    `SELECT
       tool_name,
       COUNT(*) AS span_count,
       SUM(status = 2) AS error_count,
       AVG(
         CASE
           WHEN end_unix_nano IS NOT NULL
           THEN (CAST(end_unix_nano AS REAL) - CAST(start_unix_nano AS REAL)) / 1e6
           ELSE NULL
         END
       ) AS avg_duration_ms
     FROM spans
     WHERE tool_name IS NOT NULL
     GROUP BY tool_name
     ORDER BY span_count DESC`,
  ).all<ToolStat>();
  return result.results;
}
