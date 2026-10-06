import { env, SELF } from "cloudflare:test";
import { describe, it, expect, beforeEach } from "vitest";
import worker from "../src/index";
import type { Env } from "../src/d1";
import { insertSpans, tracePage, tracePageSql } from "../src/d1";
import { flatten } from "../src/otlp";
import { applySchema } from "./schema";
import { resetDatabase } from "./reset";

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

beforeEach(async () => {
  // Vitest 4 isolates storage per file. Reset every test explicitly so data and
  // authorization assertions do not depend on execution order.
  await resetDatabase(env.DB);

  // Seed a fresh baseline for every test; POST fixtures use these same IDs.
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
  const legacy = () => SELF.fetch(`https://x/v1/query?tenant_id=personal&op=trace&trace_id=${T}`, {headers: {Authorization: BEARER}});
  const ingest = async (body: unknown) => {
    const response = await SELF.fetch("https://x/v1/traces?tenant_id=personal", {method: "POST", headers: {Authorization: BEARER, "Content-Type": "application/json"}, body: JSON.stringify(body)});
    expect(response.status).toBe(200);
  };
  const span = (id: string, start: string) => makeSingleSpanTrace(T, id.padStart(16, "0"), "synthetic", start, start);
  type Page = {rows: Array<{span_id: string; start_unix_nano: string; effective_end_unix_nano: string}>; next_cursor: string | null; complete: boolean};
  async function readAll(extra = ""): Promise<{pages: string[][]; bytes: number[]}> {
    const pages: string[][] = []; const bytes: number[] = []; let cursor: string | null = null;
    do {
      const response = await get(`${extra}${cursor ? `&cursor=${cursor}` : ""}`);
      expect(response.status).toBe(200);
      const text = await response.text();
      const page = JSON.parse(text) as Page;
      pages.push(page.rows.map(row => row.span_id)); bytes.push(new TextEncoder().encode(text).byteLength);
      expect(page.complete).toBe(page.next_cursor === null);
      cursor = page.next_cursor;
    } while (cursor);
    return {pages, bytes};
  }

  it("reconstructs a long trace with timestamp ties and trace-wide root bounds", async () => {
    await seed(205);
    let cursor: string | null = null; const ids: string[] = [];
    for (let page = 0; page < 3; page++) {
      const res = await get(cursor ? `&cursor=${cursor}` : ""); expect(res.status).toBe(200);
      const body = await res.json() as Page;
      if (page === 0) expect(body.rows[0].effective_end_unix_nano).toBe("1700000000000000204");
      ids.push(...body.rows.map(row => row.span_id)); cursor = body.next_cursor;
      expect(body.complete).toBe(page === 2);
    }
    expect(ids).toHaveLength(205); expect(new Set(ids).size).toBe(205); expect(ids).toEqual([...ids].sort());
    // Unpaged clients keep their previous allowance: a complete array up to 10,000 rows / 8 MiB.
    const old = await legacy(); expect(old.status).toBe(200);
    expect((await old.json() as unknown[]).length).toBe(205);
  });

  it("orders by numeric time where text order differs", async () => {
    await ingest(span("b", "10")); await ingest(span("a", "9"));
    expect((await readAll("&limit=1")).pages).toEqual([["000000000000000a"], ["000000000000000b"]]);
  });

  it("orders and preserves full-range unsigned timestamps", async () => {
    const times = ["1", "99", "100", "9223372036854775807", "9223372036854775808", "18446744073709551615"];
    for (const [i, start] of times.entries()) await ingest(span(String(times.length - i), start));
    const found: string[] = []; let cursor: string | null = null;
    do {
      const page = await (await get(`&limit=1${cursor ? `&cursor=${cursor}` : ""}`)).json() as Page;
      expect(page.rows[0].effective_end_unix_nano).toBe(times.at(-1));
      found.push(page.rows[0].start_unix_nano); cursor = page.next_cursor;
    } while (cursor);
    expect(found).toEqual(times);
  });

  it("returns noncanonical starts, including negative ones, without a sentinel", async () => {
    await ingest(span("1", "-5")); await ingest(span("2", "1700000000000000000"));
    const response = await get(); expect(response.status).toBe(200);
    const page = await response.json() as Page;
    expect(page.complete).toBe(true);
    expect(page.rows.map(row => row.start_unix_nano).sort()).toEqual(["-5", "1700000000000000000"]);
    expect((await (await legacy()).json() as unknown[]).length).toBe(2);
  });

  it("returns the largest row ingest accepts on its own page", async () => {
    // Quotes double once on ingest and again in the response, so this ~1 MiB
    // request is the worst case: its row alone exceeds the 2 MiB page budget.
    const big = span("2", "2");
    const attribute = {key: "note", value: {stringValue: ""}};
    (big.resourceSpans[0].scopeSpans[0].spans[0] as {attributes: unknown[]}).attributes = [attribute];
    attribute.value.stringValue = "\"".repeat((1024 * 1024 - JSON.stringify(big).length) / 2 - 1);
    expect(new TextEncoder().encode(JSON.stringify(big)).byteLength).toBeLessThanOrEqual(1024 * 1024);
    await ingest(span("1", "1")); await ingest(big); await ingest(span("3", "3"));
    const {pages, bytes} = await readAll();
    expect(pages).toEqual([["0000000000000001"], ["0000000000000002"], ["0000000000000003"]]);
    expect(bytes[1]).toBeGreaterThan(2 * 1024 * 1024);
    expect(bytes[1]).toBeLessThan(8 * 1024 * 1024);
    expect((await (await legacy()).json() as unknown[]).length).toBe(3);
  });

  it("rejects a row beyond the lone-row budget without returning its payload", async () => {
    await seed(1);
    // Only a direct write can store this; ingest bounds requests to 1 MiB.
    await env.DB.prepare("UPDATE spans SET attrs_json = ? WHERE tenant_id = ? AND trace_id = ?").bind("synthetic-canary" + "\u0001".repeat(1_500_000), "personal", T).run();
    for (const response of [await get(), await legacy()]) {
      expect(response.status).toBe(413);
      expect(await response.text()).not.toContain("synthetic-canary");
    }
  });

  it("keeps pages within 2 MiB when roots carry long trace-wide bounds", async () => {
    await seed(100);
    await env.DB.prepare("UPDATE spans SET parent_span_id = NULL WHERE tenant_id = 'personal' AND trace_id = ?").bind(T).run();
    const longEnd = "9".repeat(400 * 1024);
    const child = makeSingleSpanTrace(T, "ffffffffffffffff", "synthetic", "2", longEnd);
    Object.assign(child.resourceSpans[0].scopeSpans[0].spans[0], {parentSpanId: "0000000000000000"});
    await ingest(child);
    const {pages, bytes} = await readAll();
    expect(pages.flat()).toHaveLength(101);
    expect(Math.max(...bytes)).toBeLessThanOrEqual(2 * 1024 * 1024);
  });

  it("returns 409 when a span moves before the cursor between pages", async () => {
    // The reviewer's scenario: a@10, b@20, c@30; page 1; c re-ingested at 5.
    for (const [id, start] of [["a", "10"], ["b", "20"], ["c", "30"]]) await ingest(span(id, start));
    const first = await (await get("&limit=1")).json() as Page;
    expect(first.rows.map(row => row.span_id)).toEqual(["000000000000000a"]);
    await ingest(span("c", "5"));
    const next = await get(`&limit=1&cursor=${first.next_cursor}`);
    expect(next.status).toBe(409);
    expect(await next.json()).toEqual({error: "Trace changed during pagination; repeat the query"});
  });

  it("returns 409 for a late early parent or a deletion between pages", async () => {
    for (const [id, start] of [["a", "10"], ["b", "20"], ["c", "30"]]) await ingest(span(id, start));
    for (const mutate of [
      () => ingest(span("e", "5")),
      () => env.DB.prepare("DELETE FROM spans WHERE tenant_id = 'personal' AND span_id = '000000000000000c'").run(),
    ]) {
      const first = await (await get("&limit=1")).json() as Page;
      await mutate();
      expect((await get(`&limit=1&cursor=${first.next_cursor}`)).status).toBe(409);
    }
  });

  it("keeps reading through appends and same-position merges", async () => {
    for (const [id, start] of [["a", "10"], ["b", "20"]]) await ingest(span(id, start));
    const first = await (await get("&limit=1")).json() as Page;
    await ingest(span("b", "20")); await ingest(span("a", "10")); await ingest(span("d", "40"));
    const generation = await env.DB.prepare("SELECT generation FROM trace_generations WHERE tenant_id = 'personal' AND trace_id = ?").bind(T).first();
    expect(generation).toBeNull();
    let cursor = first.next_cursor; const ids = first.rows.map(row => row.span_id);
    while (cursor) {
      const page = await (await get(`&limit=1&cursor=${cursor}`)).json() as Page;
      ids.push(...page.rows.map(row => row.span_id)); cursor = page.next_cursor;
    }
    expect(ids).toEqual(["000000000000000a", "000000000000000b", "000000000000000d"]);
  });

  it("rejects malformed, forged and cross-context cursors and invalid limits", async () => {
    await seed(3);
    for (const extra of ["&cursor=invalid", "&limit=0", "&limit=101", "&limit=2garbage"]) expect((await get(extra)).status).toBe(400);
    const page = await (await get("&limit=1")).json() as {next_cursor: string};
    const otherTrace = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=trace&trace_id=${TRACE_ID}&page=1&cursor=${page.next_cursor}`, {headers:{Authorization:BEARER}});
    expect(otherTrace.status).toBe(400);
    const decoded = JSON.parse(atob(page.next_cursor.replace(/-/g, "+").replace(/_/g, "/")));
    const encode = (cursor: unknown) => btoa(JSON.stringify(cursor)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
    for (const forged of [{...decoded, tenant: "other"}, {...decoded, start: "abc"}, {...decoded, generation: "x"}, {...decoded, span: "missing"}]) {
      const response = await get(`&cursor=${encode(forged)}`);
      expect(response.status).toBe(400);
      expect(await response.json()).toEqual({error: "Invalid trace cursor"});
    }
    // Unknown fields are accepted but never copied into the next cursor.
    const next = await (await get(`&limit=1&cursor=${encode({...decoded, injected: "x".repeat(64)})}`)).json() as {next_cursor: string};
    expect(Object.keys(JSON.parse(atob(next.next_cursor.replace(/-/g, "+").replace(/_/g, "/")))).sort())
      .toEqual(["generation", "span", "start", "tenant", "trace", "v"]);
  });

  it("returns an empty complete page for an absent trace", async () => {
    const response = await get(); expect(response.status).toBe(200);
    expect(await response.json()).toEqual({rows: [], next_cursor: null, complete: true});
  });
});

describe("trace page cost", () => {
  // Count D1 rows_read for every statement and batch a page read issues.
  function counted(db: D1Database) {
    const stats = {rowsRead: 0};
    const real = new WeakMap<object, D1PreparedStatement>();
    const add = (result: D1Result) => { stats.rowsRead += result.meta.rows_read; };
    const wrap = (statement: D1PreparedStatement): D1PreparedStatement => {
      const proxy = new Proxy(statement, {get(target, prop) {
        if (prop === "bind") return (...values: unknown[]) => wrap(target.bind(...values));
        if (prop === "all") return async () => { const result = await target.all(); add(result); return result; };
        const value = Reflect.get(target, prop, target);
        return typeof value === "function" ? value.bind(target) : value;
      }});
      real.set(proxy, statement);
      return proxy;
    };
    const proxy = new Proxy(db, {get(target, prop) {
      if (prop === "prepare") return (sql: string) => wrap(target.prepare(sql));
      if (prop === "batch") return async (statements: D1PreparedStatement[]) => {
        const results = await target.batch(statements.map(statement => real.get(statement) ?? statement));
        results.forEach(add);
        return results;
      };
      const value = Reflect.get(target, prop, target);
      return typeof value === "function" ? value.bind(target) : value;
    }});
    return {db: proxy as D1Database, stats};
  }
  // A session root, turns under it, and tools under each turn.
  async function seed(trace: string, count: number) {
    const insert = env.DB.prepare(`INSERT INTO spans (tenant_id, span_id, source_device_id, trace_id, parent_span_id, name, kind,
      start_unix_nano, end_unix_nano, status, attrs_json, resource_json, ingested_unix_nano)
      VALUES ('personal', ?, 'mac-main', ?, ?, 'synthetic', 1, ?, ?, 0, '[]', '[]', '1')`);
    const id = (i: number) => `${trace}-${String(i).padStart(6, "0")}`;
    const statements = Array.from({length: count}, (_, i) => {
      const parent = i === 0 ? null : i % 10 === 1 ? id(0) : id(i - ((i - 1) % 10));
      const start = 1700000000000000000n + BigInt(i) * 1000n;
      return insert.bind(id(i), trace, parent, String(start), String(start + 500n));
    });
    for (let offset = 0; offset < statements.length; offset += 200) await env.DB.batch(statements.slice(offset, offset + 200));
  }
  async function fullRead(trace: string) {
    const {db, stats} = counted(env.DB);
    const counting = {...(env as unknown as Env), DB: db};
    let cursor: string | null = null; let rows = 0; const perPage: number[] = [];
    do {
      const before = stats.rowsRead;
      const page = await tracePage(counting, "personal", trace, cursor, null);
      perPage.push(stats.rowsRead - before); rows += page.rows.length; cursor = page.next_cursor;
    } while (cursor);
    return {rows, total: stats.rowsRead, perPage};
  }

  it("reads a whole trace in rows_read linear in its size", async () => {
    await seed("cost-1000", 1000); await seed("cost-2000", 2000);
    const small = await fullRead("cost-1000"); const large = await fullRead("cost-2000");
    expect(small.rows).toBe(1000); expect(large.rows).toBe(2000);
    // Before the position index each page re-read the whole trace: 59,150 vs
    // 220,239 rows_read for these fixtures. Each page now costs the same.
    expect(large.total).toBeLessThan(2.2 * small.total);
    expect(Math.max(...large.perPage)).toBeLessThan(2000);
    expect(large.perPage.at(-1)!).toBeLessThan(2 * small.perPage.at(-1)! + 100);
  });

  it("plans a page as an index seek with no sort of the trace", async () => {
    for (const afterCursor of [false, true]) {
      const plan = await env.DB.prepare(`EXPLAIN QUERY PLAN ${tracePageSql(afterCursor)}`)
        .bind("personal", "t", "1", "s", 100, 1024).all<{id: number; parent: number; detail: string}>();
      const details = plan.results.map(row => row.detail);
      expect(details).toContain(`SEARCH spans USING INDEX idx_spans_trace_position (tenant_id=? AND trace_id=?${afterCursor ? " AND <expr>>?" : ""})`);
      expect(details).toContain("SEARCH spans USING INDEX idx_spans_trace_end (tenant_id=? AND trace_id=?)");
      expect(details).toContain("SEARCH spans USING INDEX idx_spans_trace_parent (tenant_id=? AND trace_id=? AND parent_span_id=?)");
      // The only sort is the final ordering of the page's own (at most 100) rows.
      expect(plan.results.filter(row => row.detail.includes("TEMP B-TREE"))).toEqual([
        expect.objectContaining({parent: 0, detail: "USE TEMP B-TREE FOR ORDER BY"}),
      ]);
    }
  });

  it("applies schema.sql again without error", async () => {
    await applySchema(env.DB);
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
