import { env, SELF } from "cloudflare:test";
import { describe, it, expect, beforeAll } from "vitest";
import worker from "../src/index";
import type { Env } from "../src/d1";
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
  await SELF.fetch("https://x/v1/traces", {
    method: "POST",
    headers: { Authorization: BEARER, "Content-Type": "application/json" },
    body: JSON.stringify(makeOtlpFixture()),
  });
});

describe("POST /v1/traces", () => {
  it("accepts a valid OTLP payload with correct bearer and inserts spans", async () => {
    const res = await SELF.fetch("https://x/v1/traces", {
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
    await SELF.fetch("https://x/v1/traces", {
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
    const res = await SELF.fetch("https://x/v1/traces", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(makeOtlpFixture()),
    });

    expect(res.status).toBe(401);
    expect(res.headers.get("Cache-Control")).toBe("no-store");
    expect(res.headers.get("X-Content-Type-Options")).toBe("nosniff");
  });

  it("rejects with 401 when bearer token is wrong", async () => {
    const res = await SELF.fetch("https://x/v1/traces", {
      method: "POST",
      headers: {
        Authorization: "Bearer wrong-token",
        "Content-Type": "application/json",
      },
      body: JSON.stringify(makeOtlpFixture()),
    });

    expect(res.status).toBe(401);
  });

  it("rejects even Bearer blank when Worker token secret is empty", async () => {
    const res = await worker.fetch(
      new Request("https://x/v1/query?op=traces", {
        headers: { Authorization: "Bearer " },
      }),
      { DB: env.DB, GENTLY_TOKEN: "" } satisfies Env,
    );

    expect(res.status).toBe(401);
  });
});

describe("GET /v1/query", () => {
  it("op=trace returns spans for a trace ordered by start", async () => {
    const res = await SELF.fetch(
      `https://x/v1/query?op=trace&trace_id=${TRACE_ID}`,
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
    await SELF.fetch("https://x/v1/traces", {
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

    const res = await SELF.fetch(`https://x/v1/query?op=trace&trace_id=${EFF_TRACE}`, {
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
    await SELF.fetch("https://x/v1/traces", {
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
    const res = await SELF.fetch(`https://x/v1/query?op=trace&trace_id=${FT}`, {
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
    await SELF.fetch("https://x/v1/traces", {
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
    const res = await SELF.fetch(`https://x/v1/query?op=trace&trace_id=${FT}`, {
      headers: { Authorization: BEARER },
    });
    const rows = await res.json<Array<{ span_id: string; effective_end_unix_nano: string }>>();
    const turn = rows.find((r) => r.span_id === TURN)!;
    expect(turn.effective_end_unix_nano).toBe(TOOL_END); // reaches its tool child, not stuck at start
  });

  it("op=traces returns aggregated trace with correct span_count", async () => {
    const res = await SELF.fetch("https://x/v1/query?op=traces", {
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
    // One span has status=2 (error)
    expect(found?.error_count).toBe(1);
  });

  it("op=traces filters by session_id and orders by start ascending", async () => {
    const res = await SELF.fetch(
      "https://x/v1/query?op=traces&session_id=sess-abc&order=start_asc",
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
    await SELF.fetch("https://x/v1/traces", {
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

    const res = await SELF.fetch("https://x/v1/query?op=traces&order=last_activity&limit=1", {
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
      `https://x/v1/query?op=spans&trace_id=${TRACE_ID}`,
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
      `https://x/v1/query?op=spans&trace_id=${TRACE_ID}&session_id=sess-abc&harness=claude-code&order=start_asc`,
      {
        headers: { Authorization: BEARER },
      },
    );

    expect(res.status).toBe(200);
    const rows = await res.json<Array<{ span_id: string }>>();
    expect(rows.map((r) => r.span_id)).toEqual([SPAN_ID_1, SPAN_ID_2]);

    const statusRes = await SELF.fetch("https://x/v1/query?op=spans&status=2&kind=0&name=turn:1", {
      headers: { Authorization: BEARER },
    });
    expect(statusRes.status).toBe(200);
    const statusRows = await statusRes.json<Array<{ span_id: string }>>();
    expect(statusRows.map((r) => r.span_id)).toEqual([SPAN_ID_2]);
  });

  it("op=stats returns per-tool stats", async () => {
    const res = await SELF.fetch("https://x/v1/query?op=stats", {
      headers: { Authorization: BEARER },
    });

    expect(res.status).toBe(200);
    const rows = await res.json<Array<{ tool_name: string; span_count: number }>>();
    const bashStat = rows.find((r) => r.tool_name === "Bash");
    expect(bashStat).toBeDefined();
    expect(bashStat?.span_count).toBe(1);
  });

  it("unknown op returns 404", async () => {
    const res = await SELF.fetch("https://x/v1/query?op=unknown", {
      headers: { Authorization: BEARER },
    });
    expect(res.status).toBe(404);
  });
});
