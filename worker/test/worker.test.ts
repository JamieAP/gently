import { env, SELF } from "cloudflare:test";
import { describe, it, expect, beforeAll } from "vitest";
import worker from "../src/index";
import type { Env } from "../src/d1";
import { insertSpans } from "../src/d1";
import { flatten } from "../src/otlp";
import schemaSql from "../schema.sql?raw";

const BEARER = "Bearer test-token-secret";

// Fixed test fixture data
const TRACE_ID = "aabbccddeeff00112233445566778899";
const SPAN_ID_1 = "aabbccddeeff0011";
const SPAN_ID_2 = "aabbccddeeff0022";
const PARENT_SPAN_ID = "aabbccddeeff0000";
const ACTIVE_TRACE_ID = "bbccddee00112233445566778899aabb";
const ACTIVE_SPAN_ID = "bbccddee00112233";

function makeOtlpFixture() {
  return {
    resourceSpans: [
      {
        resource: {
          // session_id + harness are trace-scoped: they live on the resource,
          // exactly as the Rust encoder emits them.
          attributes: [
            { key: "service.name", value: { stringValue: "gently" } },
            { key: "gently.session_id", value: { stringValue: "sess-abc" } },
            { key: "gently.harness", value: { stringValue: "claude-code" } },
          ],
        },
        scopeSpans: [
          {
            spans: [
              {
                traceId: TRACE_ID,
                spanId: SPAN_ID_1,
                parentSpanId: PARENT_SPAN_ID,
                name: "tool:bash",
                kind: 1,
                startTimeUnixNano: "1700000000000000000",
                endTimeUnixNano: "1700000001000000000",
                attributes: [
                  { key: "gently.tool_name", value: { stringValue: "Bash" } },
                  { key: "gently.tool_use_id", value: { stringValue: "tu_001" } },
                  { key: "gently.event", value: { stringValue: "PostToolUse" } },
                ],
                status: { code: 0 },
              },
              {
                traceId: TRACE_ID,
                spanId: SPAN_ID_2,
                name: "turn:1",
                kind: 0,
                startTimeUnixNano: "1700000003000000000",
                endTimeUnixNano: "1700000004000000000",
                attributes: [],
                status: { code: 2 },
              },
            ],
          },
        ],
      },
    ],
  };
}

function makeSingleSpanTrace(
  traceId: string,
  spanId: string,
  sessionId: string,
  start: string,
  end: string,
) {
  return {
    resourceSpans: [
      {
        resource: {
          attributes: [
            { key: "service.name", value: { stringValue: "gently" } },
            { key: "gently.session_id", value: { stringValue: sessionId } },
            { key: "gently.harness", value: { stringValue: "codex" } },
          ],
        },
        scopeSpans: [
          {
            spans: [
              {
                traceId,
                spanId,
                name: "turn:active",
                kind: 1,
                startTimeUnixNano: start,
                endTimeUnixNano: end,
                attributes: [],
                status: { code: 1 },
              },
            ],
          },
        ],
      },
    ],
  };
}

beforeAll(async () => {
  // Apply schema: split on semicolons, trim, skip blanks
  const statements = schemaSql
    .split(";")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);

  for (const stmt of statements) {
    await env.DB.prepare(stmt).run();
  }

  // Seed the fixture here so it persists into every isolated test (pool-workers
  // rolls back per-test writes, but beforeAll writes form the shared baseline).
  // The query tests depend on this seed; the POST tests re-insert the same
  // span_ids, so their counts stay at 2 (INSERT OR REPLACE is idempotent).
  await SELF.fetch("https://x/v1/traces?tenant_id=personal", {
    method: "POST",
    headers: { Authorization: BEARER, "Content-Type": "application/json" },
    body: JSON.stringify(makeOtlpFixture()),
  });
});

describe("POST /v1/traces?tenant_id=personal", () => {
  it("accepts a valid OTLP payload with correct bearer and inserts spans", async () => {
    const res = await SELF.fetch("https://x/v1/traces?tenant_id=personal", {
      method: "POST",
      headers: {
        Authorization: BEARER,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(makeOtlpFixture()),
    });

    expect(res.status).toBe(200);
    const body = await res.json<{ partialSuccess: Record<string, unknown>; httpProtocol: string }>();
    expect(body).toEqual({ partialSuccess: {}, httpProtocol: "unknown" });

    const countResult = await env.DB.prepare("SELECT COUNT(*) AS n FROM spans").first<{
      n: number;
    }>();
    expect(countResult?.n).toBe(2);
  });

  it("re-posting identical spans is idempotent (INSERT OR REPLACE)", async () => {
    await SELF.fetch("https://x/v1/traces?tenant_id=personal", {
      method: "POST",
      headers: {
        Authorization: BEARER,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(makeOtlpFixture()),
    });

    const countResult = await env.DB.prepare("SELECT COUNT(*) AS n FROM spans").first<{
      n: number;
    }>();
    expect(countResult?.n).toBe(2);
  });

  it("rejects with 401 when Authorization header is missing", async () => {
    const res = await SELF.fetch("https://x/v1/traces?tenant_id=personal", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(makeOtlpFixture()),
    });

    expect(res.status).toBe(401);
    expect(res.headers.get("Cache-Control")).toBe("no-store");
    expect(res.headers.get("X-Content-Type-Options")).toBe("nosniff");
  });

  it("rejects with 401 when bearer token is wrong", async () => {
    const res = await SELF.fetch("https://x/v1/traces?tenant_id=personal", {
      method: "POST",
      headers: {
        Authorization: "Bearer wrong-token",
        "Content-Type": "application/json",
      },
      body: JSON.stringify(makeOtlpFixture()),
    });

    expect(res.status).toBe(401);
  });

  it("rejects even Bearer blank when Worker host secret is empty", async () => {
    const res = await worker.fetch(
      new Request("https://x/v1/query?tenant_id=personal&op=traces", {
        headers: { Authorization: "Bearer " },
      }),
      { DB: env.DB, GENTLY_HOSTS: "" } satisfies Env,
    );

    expect(res.status).toBe(401);
  });
});

describe("idempotent-monotonic ingest", () => {
  it("keeps merge statements compact for batched backlog drains", async () => {
    const queries: string[] = [];
    const db = {
      prepare(query: string) { queries.push(query); return env.DB.prepare(query); },
      batch: env.DB.batch.bind(env.DB),
    } as D1Database;
    await insertSpans({...env, DB: db}, "personal", "mac-main", flatten(makeOtlpFixture(), {tenant_id:"personal", device_id:"mac-main", capabilities:["ingest", "read"]}));
    const upserts = queries.filter(query => query.includes("INSERT INTO spans"));
    expect(upserts).toHaveLength(2);
    for (const query of upserts) {
      expect(new TextEncoder().encode(query).length).toBeLessThan(10_000);
      expect(query.match(/json_each/g)?.length ?? 0).toBeLessThanOrEqual(12);
    }
  });
  function spanPayload(
    spanId: string,
    traceId: string,
    start: string,
    end: string,
    statusCode: number,
    event: string,
    parentSpanId?: string,
  ) {
    return {
      resourceSpans: [
        {
          resource: { attributes: [{ key: "gently.harness", value: { stringValue: "codex" } }] },
          scopeSpans: [
            {
              spans: [
                {
                  traceId, spanId, parentSpanId,
                  name: "turn:1", kind: 1,
                  startTimeUnixNano: start, endTimeUnixNano: end,
                  attributes: [{ key: "gently.event", value: { stringValue: event } }],
                  status: { code: statusCode },
                },
              ],
            },
          ],
        },
      ],
    };
  }
  const post = (body: unknown) =>
    SELF.fetch("https://x/v1/traces?tenant_id=personal", {
      method: "POST",
      headers: { Authorization: BEARER, "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
  const readSpan = (id: string) =>
    env.DB.prepare(
      "SELECT start_unix_nano, end_unix_nano, status, attrs_json FROM spans WHERE span_id = ?",
    )
      .bind(id)
      .first<{ start_unix_nano: string; end_unix_nano: string; status: number; attrs_json: string }>();

  it("a later-delivered provisional report cannot revert a finalized span", async () => {
    const S = "1111111111111111";
    const T = "11111111111111111111111111111111";
    // finalized first ...
    await post(spanPayload(S, T, "1700000000000000000", "1700000005000000000", 1, "PostToolUse"));
    // ... then a stale/retried provisional (end == start, unset status) arrives LATE
    await post(spanPayload(S, T, "1700000000000000000", "1700000000000000000", 0, "PreToolUse"));
    const row = await readSpan(S);
    expect(row?.end_unix_nano).toBe("1700000005000000000"); // latest end kept, not reverted
    expect(row?.status).toBe(1); // content from the finalized (latest-ending) report
    expect(row?.attrs_json).toContain("PostToolUse");
    expect(row?.attrs_json).not.toContain("PreToolUse");
  });

  it("a resume re-emit keeps the earliest start (session origin) and latest end", async () => {
    const S = "2222222222222222";
    const T = "22222222222222222222222222222222";
    // original open at the session origin ...
    await post(spanPayload(S, T, "1700000100000000000", "1700000100000000000", 0, "SessionStart"));
    // ... then a resume re-emits the SAME root much later (the bug: start shoved forward)
    await post(spanPayload(S, T, "1700009999000000000", "1700009999000000000", 0, "SessionStart"));
    const row = await readSpan(S);
    expect(row?.start_unix_nano).toBe("1700000100000000000"); // earliest origin preserved
    expect(row?.end_unix_nano).toBe("1700009999000000000"); // latest activity
  });

  it("runtime tool duration survives provisional delivery and replay in either order", async () => {
    const T = "33333333333333333333333333333333";
    const toolPayload = (id: string, closed: boolean) => {
      const body = spanPayload(id, T, closed ? "1700000000200000000" : "1700000000000000000",
        closed ? "1700000000300000000" : "1700000000000000000", closed ? 2 : 0,
        closed ? "PostToolUseFailure" : "PreToolUse");
      const span = body.resourceSpans[0].scopeSpans[0].spans[0];
      span.name = "TimedTool";
      span.kind = 3;
      span.attributes.push({key: "gently.tool_name", value: {stringValue: "TimedTool"}},
        {key: "gently.tool_state", value: {stringValue: closed ? "closed" : "open"}});
      if (closed) span.attributes.push({key: "gently.tool_duration_ms", value: {stringValue: "100"}});
      return body;
    };
    for (const [id, order] of [["3333333333333301", [false, true, false]], ["3333333333333302", [true, false, true]]] as const) {
      for (const closed of order) expect((await post(toolPayload(id, closed))).status).toBe(200);
      const row = await readSpan(id);
      expect(row?.start_unix_nano).toBe("1700000000200000000");
      expect(row?.end_unix_nano).toBe("1700000000300000000");
      expect(row?.status).toBe(2);
      expect(row?.attrs_json).toContain("closed");
    }
    const res = await SELF.fetch("https://x/v1/query?tenant_id=personal&op=stats", {headers: {Authorization: BEARER}});
    const rows = await res.json<Array<{tool_name: string; span_count: number; avg_duration_ms: number}>>();
    expect(rows.find(r => r.tool_name === "TimedTool")).toMatchObject({span_count: 2, avg_duration_ms: 100});
  });

  it("unfinished tool invocations count once but do not dilute completed duration averages", async () => {
    const T = "44444444444444444444444444444444";
    const toolPayload = (id: string, closed: boolean) => {
      const body = spanPayload(id, T, "1700000000000000000",
        closed ? "1700000000100000000" : "1700000000000000000", 0,
        closed ? "PostToolUse" : "PreToolUse");
      const span = body.resourceSpans[0].scopeSpans[0].spans[0];
      span.name = "OpaqueTool";
      span.kind = 3;
      span.attributes.push({key: "gently.tool_name", value: {stringValue: "OpaqueTool"}},
        {key: "gently.tool_state", value: {stringValue: closed ? "closed" : "open"}});
      return body;
    };
    await post(toolPayload("4444444444444401", false));
    await post(toolPayload("4444444444444401", false));
    let res = await SELF.fetch("https://x/v1/query?tenant_id=personal&op=stats", {headers: {Authorization: BEARER}});
    let rows = await res.json<Array<{tool_name: string; span_count: number; avg_duration_ms: number | null}>>();
    expect(rows.find(r => r.tool_name === "OpaqueTool")).toMatchObject({span_count: 1, avg_duration_ms: null});
    await post(toolPayload("4444444444444402", true));
    res = await SELF.fetch("https://x/v1/query?tenant_id=personal&op=stats", {headers: {Authorization: BEARER}});
    rows = await res.json();
    expect(rows.find(r => r.tool_name === "OpaqueTool")).toMatchObject({span_count: 2, avg_duration_ms: 100});
  });

  it("a delayed open can recover a missing tool start without reverting completion", async () => {
    const id = "5555555555555501";
    const T = "55555555555555555555555555555555";
    const body = (closed: boolean) => {
      const payload = spanPayload(id, T, closed ? "1700000000200000000" : "1700000000000000000",
        closed ? "1700000000200000000" : "1700000000000000000", 0,
        closed ? "PostToolUse" : "PreToolUse");
      const span = payload.resourceSpans[0].scopeSpans[0].spans[0];
      span.name = "DelayedTool";
      span.kind = 3;
      span.attributes.push({key: "gently.tool_name", value: {stringValue: "DelayedTool"}},
        {key: "gently.tool_state", value: {stringValue: closed ? "closed" : "open"}});
      return payload;
    };
    await post(body(true));
    await post(body(false));
    const row = await readSpan(id);
    expect(row?.start_unix_nano).toBe("1700000000000000000");
    expect(row?.end_unix_nano).toBe("1700000000200000000");
    expect(row?.attrs_json).toContain("closed");
  });

  it("classifies duplicated imported tool attributes consistently on insert and replay", async () => {
    const id = "6666666666666601";
    const T = "66666666666666666666666666666666";
    const open = spanPayload(id, T, "1700000000000000000", "1700000000000000000", 0, "PreToolUse");
    const closed = spanPayload(id, T, "1700000000200000000", "1700000000300000000", 2, "PostToolUseFailure");
    for (const payload of [open, closed]) {
      const span = payload.resourceSpans[0].scopeSpans[0].spans[0];
      span.name = "ImportedTool";
      span.kind = 3;
      span.attributes.push({key: "gently.tool_name", value: {stringValue: "ImportedTool"}},
        {key: "gently.tool_state", value: {stringValue: payload === open ? "open" : "closed"}});
    }
    const attrs = closed.resourceSpans[0].scopeSpans[0].spans[0].attributes;
    attrs.unshift({key: "gently.event", value: {stringValue: "PreToolUse"}},
      {key: "gently.tool_state", value: {stringValue: "open"}});
    attrs.push({key: "gently.tool_duration_ms", value: {stringValue: "100"}});
    await post(open);
    await post(closed);
    await post(open);
    const row = await readSpan(id);
    expect(row?.start_unix_nano).toBe("1700000000200000000");
    expect(row?.end_unix_nano).toBe("1700000000300000000");
    expect(row?.status).toBe(2);
    const res = await SELF.fetch("https://x/v1/query?tenant_id=personal&op=stats", {headers: {Authorization: BEARER}});
    const rows = await res.json<Array<{tool_name: string; avg_duration_ms: number | null}>>();
    expect(rows.find(r => r.tool_name === "ImportedTool")?.avg_duration_ms).toBe(100);
  });
});

describe("GET /v1/query", () => {
  it("legacy permission markers remain queryable without inflating tool counts or failures", async () => {
    const T = "77777777777777777777777777777777";
    const common = {traceId:T, name:"LegacyRollupTool", kind:3,
      startTimeUnixNano:"1700000000000000000", endTimeUnixNano:"1700000000100000000"};
    const toolAttrs = [{key:"gently.tool_name",value:{stringValue:"LegacyRollupTool"}},
      {key:"gently.tool_use_id",value:{stringValue:"legacy-call"}}];
    const report = (id: string, event: string, status: number, marker: boolean) => ({
      ...common, spanId:id, name:marker ? event : common.name, kind:marker ? 1 : 3,
      endTimeUnixNano:marker ? common.startTimeUnixNano : common.endTimeUnixNano,
      status:{code:status}, attributes:[...toolAttrs,{key:"gently.event",value:{stringValue:event}}],
    });
    const ingest = await SELF.fetch("https://x/v1/traces?tenant_id=personal", {method:"POST",
      headers:{Authorization:BEARER,"Content-Type":"application/json"},
      body:JSON.stringify({resourceSpans:[{resource:{attributes:[{key:"gently.session_id",value:{stringValue:"legacy-permissions"}}]},scopeSpans:[{spans:[
        report("7777777777777701","PostToolUseFailure",2,false),
        report("7777777777777702","PermissionRequest",0,true),
        report("7777777777777703","PermissionDenied",2,true),
      ]}]}]}),
    });
    expect(ingest.status).toBe(200);
    const statsRes = await SELF.fetch("https://x/v1/query?tenant_id=personal&op=stats",{headers:{Authorization:BEARER}});
    const groups = await statsRes.json<Array<{tool_name:string;span_count:number;error_count:number;avg_duration_ms:number}>>();
    expect(groups.find(r => r.tool_name === "LegacyRollupTool")).toMatchObject({span_count:1,error_count:1,avg_duration_ms:100});
    const tracesRes = await SELF.fetch("https://x/v1/query?tenant_id=personal&op=traces&session_id=legacy-permissions",{headers:{Authorization:BEARER}});
    const traces = await tracesRes.json<Array<{span_count:number;error_count:number}>>();
    expect(traces).toHaveLength(1);
    expect(traces[0]).toMatchObject({span_count:3,error_count:1});
    const traceRes = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=trace&trace_id=${T}`,{headers:{Authorization:BEARER}});
    const rows = await traceRes.json<Array<{name:string}>>();
    expect(rows.map(r => r.name)).toEqual(expect.arrayContaining(["PermissionRequest","PermissionDenied","LegacyRollupTool"]));
  });

  it("op=trace returns spans for a trace ordered by start", async () => {
    const res = await SELF.fetch(
      `https://x/v1/query?tenant_id=personal&op=trace&trace_id=${TRACE_ID}`,
      {
        headers: { Authorization: BEARER },
      },
    );

    expect(res.status).toBe(200);
    expect(res.headers.get("Cache-Control")).toBe("no-store");
    expect(res.headers.get("X-Content-Type-Options")).toBe("nosniff");
    const rows = await res.json<Array<{ span_id: string; trace_id: string }>>();
    expect(rows.length).toBe(2);
    const spanIds = rows.map((r) => r.span_id);
    expect(spanIds).toContain(SPAN_ID_1);
    expect(spanIds).toContain(SPAN_ID_2);
  });

  it("op=trace derives effective_end from descendants for a provisional parent", async () => {
    // A Codex-style session root that never closed (end == start), with a child
    // tool that ran later. The stored root width is zero; the derived effective
    // end must reach the child's end. nanos exceed 2^53 - assert as strings.
    const EFF_TRACE = "ddeeff00112233445566778899aabbcc";
    const ROOT = "ddeeff0011223300";
    const CHILD = "ddeeff0011223311";
    const ROOT_START = "1700000100000000000";
    const CHILD_END = "1700000900000000000";
    await SELF.fetch("https://x/v1/traces?tenant_id=personal", {
      method: "POST",
      headers: { Authorization: BEARER, "Content-Type": "application/json" },
      body: JSON.stringify({
        resourceSpans: [
          {
            resource: {
              attributes: [
                { key: "gently.session_id", value: { stringValue: "sess-eff" } },
                { key: "gently.harness", value: { stringValue: "codex" } },
              ],
            },
            scopeSpans: [
              {
                spans: [
                  {
                    traceId: EFF_TRACE,
                    spanId: ROOT,
                    name: "session",
                    kind: 1,
                    startTimeUnixNano: ROOT_START,
                    endTimeUnixNano: ROOT_START, // provisional: never finalized
                    status: { code: 0 },
                  },
                  {
                    traceId: EFF_TRACE,
                    spanId: CHILD,
                    parentSpanId: ROOT,
                    name: "Bash",
                    kind: 3,
                    startTimeUnixNano: "1700000200000000000",
                    endTimeUnixNano: CHILD_END,
                    attributes: [{ key: "gently.tool_name", value: { stringValue: "Bash" } }],
                    status: { code: 1 },
                  },
                ],
              },
            ],
          },
        ],
      }),
    });

    const res = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=trace&trace_id=${EFF_TRACE}`, {
      headers: { Authorization: BEARER },
    });
    expect(res.status).toBe(200);
    const rows = await res.json<
      Array<{ span_id: string; end_unix_nano: string | null; effective_end_unix_nano: string }>
    >();
    const root = rows.find((r) => r.span_id === ROOT)!;
    const child = rows.find((r) => r.span_id === CHILD)!;
    // Root's stored end is still provisional, but its derived end reaches the child.
    expect(root.end_unix_nano).toBe(ROOT_START);
    expect(root.effective_end_unix_nano).toBe(CHILD_END);
    // A finalized leaf is unaffected: its effective end is its own end.
    expect(child.effective_end_unix_nano).toBe(CHILD_END);
  });

  it("op=trace leaves a finalized parent's end untouched (no provisional recursion)", async () => {
    // A finalized parent (end > start) that ends AFTER its child must keep its
    // own end - it is NOT anchored, so the recursion never runs for it. This
    // guards the fix that anchors recursion only on provisional spans.
    const FT = "eeff00112233445566778899aabbccdd";
    const P = "eeff001122334400";
    const C = "eeff001122334411";
    const P_END = "1700001000000000000"; // parent ends last
    await SELF.fetch("https://x/v1/traces?tenant_id=personal", {
      method: "POST",
      headers: { Authorization: BEARER, "Content-Type": "application/json" },
      body: JSON.stringify({
        resourceSpans: [
          {
            resource: { attributes: [{ key: "gently.harness", value: { stringValue: "claude-code" } }] },
            scopeSpans: [
              {
                spans: [
                  {
                    traceId: FT, spanId: P, name: "session", kind: 1,
                    startTimeUnixNano: "1700000500000000000", endTimeUnixNano: P_END,
                    status: { code: 1 },
                  },
                  {
                    traceId: FT, spanId: C, parentSpanId: P, name: "turn:1", kind: 1,
                    startTimeUnixNano: "1700000600000000000", endTimeUnixNano: "1700000700000000000",
                    status: { code: 1 },
                  },
                ],
              },
            ],
          },
        ],
      }),
    });
    const res = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=trace&trace_id=${FT}`, {
      headers: { Authorization: BEARER },
    });
    const rows = await res.json<Array<{ span_id: string; effective_end_unix_nano: string }>>();
    const parent = rows.find((r) => r.span_id === P)!;
    expect(parent.effective_end_unix_nano).toBe(P_END); // own end, not the child's earlier end
  });

  it("op=trace derives a provisional non-root turn from its direct children", async () => {
    // A finalized session root, a provisional (interrupted) turn under it, and a
    // finalized tool under the turn. The turn must take its child's end.
    const FT = "ff00112233445566778899aabbccddee";
    const ROOT = "ff0011223344aa00";
    const TURN = "ff0011223344aa11"; // provisional: end == start
    const TOOL = "ff0011223344aa22";
    const TURN_START = "1700002000000000000";
    const TOOL_END = "1700002800000000000";
    await SELF.fetch("https://x/v1/traces?tenant_id=personal", {
      method: "POST",
      headers: { Authorization: BEARER, "Content-Type": "application/json" },
      body: JSON.stringify({
        resourceSpans: [
          {
            resource: { attributes: [{ key: "gently.harness", value: { stringValue: "codex" } }] },
            scopeSpans: [
              {
                spans: [
                  { traceId: FT, spanId: ROOT, name: "session", kind: 1, startTimeUnixNano: "1700002000000000000", endTimeUnixNano: "1700003000000000000", status: { code: 1 } },
                  { traceId: FT, spanId: TURN, parentSpanId: ROOT, name: "turn:1", kind: 1, startTimeUnixNano: TURN_START, endTimeUnixNano: TURN_START, status: { code: 0 } },
                  { traceId: FT, spanId: TOOL, parentSpanId: TURN, name: "Bash", kind: 3, startTimeUnixNano: "1700002100000000000", endTimeUnixNano: TOOL_END, status: { code: 1 } },
                ],
              },
            ],
          },
        ],
      }),
    });
    const res = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=trace&trace_id=${FT}`, {
      headers: { Authorization: BEARER },
    });
    const rows = await res.json<Array<{ span_id: string; effective_end_unix_nano: string }>>();
    const turn = rows.find((r) => r.span_id === TURN)!;
    expect(turn.effective_end_unix_nano).toBe(TOOL_END); // reaches its tool child, not stuck at start
  });

  it("op=trace derives effective_start when a turn opens AFTER its own children", async () => {
    // The Codex shape: a tool keyed to a turn starts before that turn's span
    // (UserPromptSubmit/Stop land late). The turn's effective_start must reach
    // back to its earliest child so the child nests within it.
    const FT = "00112233445566778899aabbccddeeff";
    const ROOT = "00112233445566aa";
    const TURN = "00112233445566bb"; // opens late
    const TOOL = "00112233445566cc"; // starts before the turn
    const TOOL_START = "1700004000000000000";
    const TURN_START = "1700004500000000000"; // 500s AFTER its tool
    await SELF.fetch("https://x/v1/traces?tenant_id=personal", {
      method: "POST",
      headers: { Authorization: BEARER, "Content-Type": "application/json" },
      body: JSON.stringify({
        resourceSpans: [
          {
            resource: { attributes: [{ key: "gently.harness", value: { stringValue: "codex" } }] },
            scopeSpans: [
              {
                spans: [
                  { traceId: FT, spanId: ROOT, name: "session", kind: 1, startTimeUnixNano: "1700004000000000000", endTimeUnixNano: "1700004000000000000", status: { code: 0 } },
                  { traceId: FT, spanId: TURN, parentSpanId: ROOT, name: "turn:6", kind: 1, startTimeUnixNano: TURN_START, endTimeUnixNano: "1700004600000000000", status: { code: 1 } },
                  { traceId: FT, spanId: TOOL, parentSpanId: TURN, name: "Bash", kind: 3, startTimeUnixNano: TOOL_START, endTimeUnixNano: "1700004550000000000", status: { code: 1 } },
                ],
              },
            ],
          },
        ],
      }),
    });
    const res = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=trace&trace_id=${FT}`, {
      headers: { Authorization: BEARER },
    });
    const rows = await res.json<Array<{ span_id: string; effective_start_unix_nano: string }>>();
    const turn = rows.find((r) => r.span_id === TURN)!;
    const root = rows.find((r) => r.span_id === ROOT)!;
    expect(turn.effective_start_unix_nano).toBe(TOOL_START); // reaches back to its earliest child
    expect(root.effective_start_unix_nano).toBe(TOOL_START); // session root = trace min
  });

  it("a resumed session root (end != start) still spans the whole trace", async () => {
    // Monotonic ingest sets a resumed root's end to the latest SessionStart, so
    // end != start even though it never truly closed. effective_end must still be
    // the trace max (a child ending later), not the stale stored end.
    const FT = "aa00112233445566778899aabbccddff";
    const ROOT = "aa001122334455a0";
    const TOOL = "aa001122334455a1";
    const ROOT_START = "1700005000000000000";
    const ROOT_STORED_END = "1700005100000000000"; // last SessionStart (looks "finalized")
    const TOOL_END = "1700005900000000000"; // a child ran well past the root's stored end
    await SELF.fetch("https://x/v1/traces?tenant_id=personal", {
      method: "POST",
      headers: { Authorization: BEARER, "Content-Type": "application/json" },
      body: JSON.stringify({
        resourceSpans: [
          {
            resource: { attributes: [{ key: "gently.harness", value: { stringValue: "codex" } }] },
            scopeSpans: [
              {
                spans: [
                  { traceId: FT, spanId: ROOT, name: "session", kind: 1, startTimeUnixNano: ROOT_START, endTimeUnixNano: ROOT_STORED_END, status: { code: 0 } },
                  { traceId: FT, spanId: TOOL, parentSpanId: ROOT, name: "Bash", kind: 3, startTimeUnixNano: "1700005200000000000", endTimeUnixNano: TOOL_END, status: { code: 1 } },
                ],
              },
            ],
          },
        ],
      }),
    });
    const res = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=trace&trace_id=${FT}`, {
      headers: { Authorization: BEARER },
    });
    const rows = await res.json<Array<{ span_id: string; effective_end_unix_nano: string }>>();
    const root = rows.find((r) => r.span_id === ROOT)!;
    expect(root.effective_end_unix_nano).toBe(TOOL_END); // trace max, not the stale stored end
  });

  it("op=traces returns aggregated trace with correct span_count", async () => {
    const res = await SELF.fetch("https://x/v1/query?tenant_id=personal&op=traces", {
      headers: { Authorization: BEARER },
    });

    expect(res.status).toBe(200);
    const rows = await res.json<
      Array<{ trace_id: string; span_count: number; error_count: number }>
    >();

    const found = rows.find((r) => r.trace_id === TRACE_ID) as
      | { trace_id: string; span_count: number; error_count: number; session_id?: string; harness?: string }
      | undefined;
    expect(found).toBeDefined();
    expect(found?.span_count).toBe(2);
    // trace-scoped attrs lifted from the resource, not the span
    expect(found?.session_id).toBe("sess-abc");
    expect(found?.harness).toBe("claude-code");
    // The only status=2 span here is the turn (a StopFailure), which has no
    // tool_name - error_count counts failed TOOL calls only, so it is 0.
    expect(found?.error_count).toBe(0);
  });

  it("op=traces error_count counts failed tool calls, not StopFailure turns", async () => {
    // A turn that aborts (StopFailure) and a tool call that fails both carry
    // status=2. Only the tool failure is an "error" for the summary count.
    const traceId = "cccccccccccccccccccccccccccccccc";
    const sess = "sess-errcount";
    const fixture = {
      resourceSpans: [
        {
          resource: {
            attributes: [
              { key: "service.name", value: { stringValue: "gently" } },
              { key: "gently.session_id", value: { stringValue: sess } },
              { key: "gently.harness", value: { stringValue: "claude-code" } },
            ],
          },
          scopeSpans: [
            {
              spans: [
                {
                  traceId,
                  spanId: "c0000000000000a1",
                  name: "tool:bash",
                  kind: 1,
                  startTimeUnixNano: "1700000010000000000",
                  endTimeUnixNano: "1700000011000000000",
                  attributes: [
                    { key: "gently.tool_name", value: { stringValue: "Bash" } },
                  ],
                  status: { code: 2 }, // failed tool call → counts
                },
                {
                  traceId,
                  spanId: "c0000000000000a2",
                  name: "turn:7",
                  kind: 0,
                  startTimeUnixNano: "1700000012000000000",
                  endTimeUnixNano: "1700000013000000000",
                  attributes: [],
                  status: { code: 2 }, // StopFailure turn → must NOT count
                },
              ],
            },
          ],
        },
      ],
    };

    await SELF.fetch("https://x/v1/traces?tenant_id=personal", {
      method: "POST",
      headers: { Authorization: BEARER, "Content-Type": "application/json" },
      body: JSON.stringify(fixture),
    });

    const res = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=traces&session_id=${sess}`, {
      headers: { Authorization: BEARER },
    });
    expect(res.status).toBe(200);
    const rows = await res.json<Array<{ trace_id: string; error_count: number }>>();
    const found = rows.find((r) => r.trace_id === traceId);
    expect(found?.error_count).toBe(1);
  });

  it("op=traces filters by session_id and orders by start ascending", async () => {
    const res = await SELF.fetch(
      "https://x/v1/query?tenant_id=personal&op=traces&session_id=sess-abc&order=start_asc",
      {
        headers: { Authorization: BEARER },
      },
    );

    expect(res.status).toBe(200);
    const rows = await res.json<Array<{ trace_id: string; session_id: string }>>();
    expect(rows).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ trace_id: TRACE_ID, session_id: "sess-abc" }),
      ]),
    );
  });

  it("op=traces supports last_activity ordering", async () => {
    await SELF.fetch("https://x/v1/traces?tenant_id=personal", {
      method: "POST",
      headers: { Authorization: BEARER, "Content-Type": "application/json" },
      body: JSON.stringify(
        makeSingleSpanTrace(
          ACTIVE_TRACE_ID,
          ACTIVE_SPAN_ID,
          "sess-active",
          "1699999990000000000",
          "1700000010000000000",
        ),
      ),
    });

    const res = await SELF.fetch("https://x/v1/query?tenant_id=personal&op=traces&order=last_activity&limit=1", {
      headers: { Authorization: BEARER },
    });

    expect(res.status).toBe(200);
    const rows = await res.json<Array<{ trace_id: string; last_activity: string }>>();
    expect(rows).toEqual([
      expect.objectContaining({
        trace_id: ACTIVE_TRACE_ID,
        last_activity: "1700000010000000000",
      }),
    ]);
  });

  it("op=spans returns filtered spans by trace_id", async () => {
    const res = await SELF.fetch(
      `https://x/v1/query?tenant_id=personal&op=spans&trace_id=${TRACE_ID}`,
      {
        headers: { Authorization: BEARER },
      },
    );

    expect(res.status).toBe(200);
    const rows = await res.json<Array<{ span_id: string }>>();
    expect(rows.length).toBe(2);
  });

  it("op=spans supports indexed filters and explicit order", async () => {
    const res = await SELF.fetch(
      `https://x/v1/query?tenant_id=personal&op=spans&trace_id=${TRACE_ID}&session_id=sess-abc&harness=claude-code&order=start_asc`,
      {
        headers: { Authorization: BEARER },
      },
    );

    expect(res.status).toBe(200);
    const rows = await res.json<Array<{ span_id: string }>>();
    expect(rows.map((r) => r.span_id)).toEqual([SPAN_ID_1, SPAN_ID_2]);

    const statusRes = await SELF.fetch("https://x/v1/query?tenant_id=personal&op=spans&status=2&kind=0&name=turn:1", {
      headers: { Authorization: BEARER },
    });
    expect(statusRes.status).toBe(200);
    const statusRows = await statusRes.json<Array<{ span_id: string }>>();
    expect(statusRows.map((r) => r.span_id)).toEqual([SPAN_ID_2]);
  });

  it("op=stats returns per-tool stats", async () => {
    const res = await SELF.fetch("https://x/v1/query?tenant_id=personal&op=stats", {
      headers: { Authorization: BEARER },
    });

    expect(res.status).toBe(200);
    const rows = await res.json<Array<{ tool_name: string; span_count: number }>>();
    const bashStat = rows.find((r) => r.tool_name === "Bash");
    expect(bashStat).toBeDefined();
    expect(bashStat?.span_count).toBe(1);
  });

  it("unknown op returns 404", async () => {
    const res = await SELF.fetch("https://x/v1/query?tenant_id=personal&op=unknown", {
      headers: { Authorization: BEARER },
    });
    expect(res.status).toBe(404);
  });
});


describe("bounded trace pages", () => {
  const T = "9876543210abcdef9876543210abcdef";
  async function seed(count: number) {
    const rows = Array.from({length: count}, (_, i) => ({
      tenant_id: "personal", device_id: "mac-main", trace_id: T,
      span_id: i.toString(16).padStart(16, "0"), parent_span_id: i ? "0000000000000000" : null,
      name: "synthetic", kind: 1, start_unix_nano: "1700000000000000000",
      end_unix_nano: String(1700000000000000000n + BigInt(i)), status: 0,
      session_id: "synthetic", harness: "codex", tool_name: null, tool_use_id: null,
      attrs_json: "[]", resource_json: "[]", ingested_unix_nano: "1700000000000000000"
    }));
    for (let i = 0; i < count; i += 32) await insertSpans(env, "personal", "mac-main", rows.slice(i, i + 32));
  }
  const get = (extra = "") => SELF.fetch(`https://x/v1/query?tenant_id=personal&op=trace&trace_id=${T}&page=1${extra}`, {headers: {Authorization: BEARER}});
  it("reconstructs a long trace with timestamp ties and trace-wide root bounds", async () => {
    await seed(205);
    let cursor: string | null = null; const ids: string[] = [];
    for (let page = 0; page < 3; page++) {
      const res = await get(cursor ? `&cursor=${cursor}` : ""); expect(res.status).toBe(200);
      const body = await res.json() as {rows: Array<{span_id: string; effective_end_unix_nano: string}>; next_cursor: string | null; complete: boolean};
      if (page === 0) expect(body.rows[0].effective_end_unix_nano).toBe("1700000000000000204");
      ids.push(...body.rows.map(row => row.span_id)); cursor = body.next_cursor;
      expect(body.complete).toBe(page === 2);
    }
    expect(ids).toHaveLength(205); expect(new Set(ids).size).toBe(205); expect(ids).toEqual([...ids].sort());
    const legacy = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=trace&trace_id=${T}`, {headers:{Authorization:BEARER}});
    expect(legacy.status).toBe(413);
  });
  it("rejects malformed and cross-context cursors and invalid limits", async () => {
    await seed(2);
    for (const extra of ["&cursor=invalid", "&limit=0", "&limit=101", "&limit=2garbage"]) expect((await get(extra)).status).toBe(400);
    const page = await (await get("&limit=1")).json() as {next_cursor: string};
    const otherTrace = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=trace&trace_id=${TRACE_ID}&page=1&cursor=${page.next_cursor}`, {headers:{Authorization:BEARER}});
    expect(otherTrace.status).toBe(400);
    const foreign = btoa(JSON.stringify({v:1,tenant:"other",trace:T,start:"1700000000000000000",span:"0000000000000000"})).replace(/=+$/, "");
    expect((await get(`&cursor=${foreign}`)).status).toBe(400);
  });
  it("rejects a single row beyond the byte budget without returning its payload", async () => {
    await seed(1);
    await env.DB.prepare("UPDATE spans SET attrs_json = ? WHERE tenant_id = ? AND trace_id = ?").bind("synthetic-canary".repeat(100_000), "personal", T).run();
    const response = await get(); expect(response.status).toBe(413);
    expect(await response.text()).not.toContain("synthetic-canary");
  });
});


describe("trace continuation compatibility", () => {
  it("continues accepted uppercase IDs, unicode legacy IDs and uint64 timestamps", async () => {
    const T = "ABCDEF0123456789ABCDEF0123456789";
    const ids = ["ABCDEF012345678A", "ABCDEF012345678B", "legacy-µ"];
    for (const spanId of ids) {
      const body = makeSingleSpanTrace(T, spanId, "synthetic-uppercase", "18446744073709551615", "18446744073709551615");
      const post = await SELF.fetch("https://x/v1/traces?tenant_id=personal",{method:"POST",headers:{Authorization:BEARER,"Content-Type":"application/json"},body:JSON.stringify(body)});
      expect(post.status).toBe(200);
    }
    let cursor: string | null = null; const found: string[] = [];
    for (let i=0; i<3; i++) {
      const response = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=trace&trace_id=${T}&page=1&limit=1${cursor ? `&cursor=${cursor}` : ""}`,{headers:{Authorization:BEARER}});
      expect(response.status).toBe(200);
      const page = await response.json() as {rows:Array<{span_id:string}>;next_cursor:string|null};
      found.push(page.rows[0].span_id); cursor=page.next_cursor;
    }
    expect(found).toEqual(ids); expect(cursor).toBeNull();
  });
});
