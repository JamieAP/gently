import { env, SELF } from "cloudflare:test";
import { describe, it, expect, beforeAll } from "vitest";
import schemaSql from "../schema.sql?raw";

const BEARER = "Bearer test-token-secret";

// Fixed test fixture data
const TRACE_ID = "aabbccddeeff00112233445566778899";
const SPAN_ID_1 = "aabbccddeeff0011";
const SPAN_ID_2 = "aabbccddeeff0022";
const PARENT_SPAN_ID = "aabbccddeeff0000";

function makeOtlpFixture() {
  return {
    resourceSpans: [
      {
        resource: {
          attributes: [
            { key: "service.name", value: { stringValue: "gently" } },
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
                  { key: "gently.session_id", value: { stringValue: "sess-abc" } },
                  { key: "gently.harness", value: { stringValue: "claude-code" } },
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
                startTimeUnixNano: "1700000000000000000",
                endTimeUnixNano: "1700000002000000000",
                attributes: [
                  { key: "gently.session_id", value: { stringValue: "sess-abc" } },
                  { key: "gently.harness", value: { stringValue: "claude-code" } },
                ],
                status: { code: 2 },
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
    const body = await res.json<{ partialSuccess: Record<string, unknown> }>();
    expect(body).toEqual({ partialSuccess: {} });

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
    const rows = await res.json<Array<{ span_id: string; trace_id: string }>>();
    expect(rows.length).toBe(2);
    const spanIds = rows.map((r) => r.span_id);
    expect(spanIds).toContain(SPAN_ID_1);
    expect(spanIds).toContain(SPAN_ID_2);
  });

  it("op=traces returns aggregated trace with correct span_count", async () => {
    const res = await SELF.fetch("https://x/v1/query?op=traces", {
      headers: { Authorization: BEARER },
    });

    expect(res.status).toBe(200);
    const rows = await res.json<
      Array<{ trace_id: string; span_count: number; error_count: number }>
    >();

    const found = rows.find((r) => r.trace_id === TRACE_ID);
    expect(found).toBeDefined();
    expect(found?.span_count).toBe(2);
    // One span has status=2 (error)
    expect(found?.error_count).toBe(1);
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
