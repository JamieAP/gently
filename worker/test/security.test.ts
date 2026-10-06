import { env, SELF } from "cloudflare:test";
import { beforeAll, describe, expect, it } from "vitest";
import worker from "../src/index";
import type { Env } from "../src/d1";
import { applySchema } from "./schema";

const TOKEN = "test-token-secret";
const OTHER = "other-tenant-token";
const INGEST = "ingest-only-token";
const READ = "read-only-token";
const RAW_REF = "a1".repeat(16);
// Genuine age ciphertext for harmless fixture text, encrypted to a public example recipient.
const CIPHERTEXT = "YWdlLWVuY3J5cHRpb24ub3JnL3YxCi0+IFgyNTUxOSBBWU5rZCtGMktPc1VJMWNkdlh0ZDBxTCtoWkpBd3pIRHNGS3E2ZXZVTmcwCldCSk1oTW92aWV1OElSODZBbjZLS2tQNG50NGJET0wwSkkwU0dqTWFONzAKLS0tIEF6ek92MHdhOUNHN1cwV2UwQTBRT2R0cWd3ZjRYMy85aWVuVmtXQTRJN0kKoG86JfRL0fu0x/8m1gcJ46ul1kUNmVDNzwjMtWPqmzJUCT/oM0DZF4tXVKnQGHZy4KwBWCo5Na9xPpyO7lsAu/kSHg==";

function headers(token = TOKEN): HeadersInit {
  return { Authorization: `Bearer ${token}`, "Content-Type": "application/json" };
}
function rawObject(overrides: Record<string, unknown> = {}) {
  return {
    version: 1,
    context: {
      tenant_id: "personal", device_id: "mac-main", key_epoch: 1,
      raw_ref: RAW_REF, session_id: "session-fixture", harness: "codex", event: "UserPromptSubmit",
    },
    ciphertext_b64: CIPHERTEXT,
    ...overrides,
  };
}
function postRaw(object = rawObject(), token = TOKEN, tenant = "personal") {
  return SELF.fetch(`https://x/v1/raw-values?tenant_id=${tenant}`, {
    method: "POST", headers: headers(token), body: JSON.stringify(object),
  });
}
function getRaw(ref = RAW_REF, token = TOKEN, tenant = "personal") {
  return SELF.fetch(`https://x/v1/raw-values/${ref}?tenant_id=${tenant}`, { headers: headers(token) });
}
function otlp(attributes: unknown[] = [], resourceAttributes: unknown[] = []) {
  return { resourceSpans: [{ resource: { attributes: resourceAttributes }, scopeSpans: [{ spans: [{
    traceId: "11".repeat(16), spanId: "22".repeat(8), name: "tool:bash", kind: 1,
    startTimeUnixNano: "1700000000000000000", endTimeUnixNano: "1700000001000000000",
    attributes: [{ key: "gently.tool_name", value: { stringValue: "Bash" } }, ...attributes],
  }] }] }] };
}
function postSpans(body: unknown, token = TOKEN, tenant = "personal") {
  return SELF.fetch(`https://x/v1/traces?tenant_id=${tenant}`, {
    method: "POST", headers: headers(token), body: JSON.stringify(body),
  });
}

beforeAll(async () => {
  await applySchema(env.DB);
});

describe("tenant and device authorization", () => {
  it("requires a tenant on every authenticated route", async () => {
    for (const path of ["/v1/query?op=traces", "/v1/whoami", `/v1/raw-values/${RAW_REF}`]) {
      const response = await SELF.fetch(`https://x${path}`, { headers: headers() });
      expect(response.status).toBe(400);
    }
  });
  it("denies a request for another tenant", async () => {
    expect((await SELF.fetch("https://x/v1/query?tenant_id=other&op=traces", { headers: headers() })).status).toBe(403);
    expect((await getRaw(RAW_REF, TOKEN, "other")).status).toBe(403);
  });
  it("ingest-only hosts cannot read metadata or ciphertext", async () => {
    expect((await SELF.fetch("https://x/v1/query?tenant_id=personal&op=traces", { headers: headers(INGEST) })).status).toBe(403);
    expect((await getRaw(RAW_REF, INGEST)).status).toBe(403);
    expect((await postSpans(otlp(), INGEST)).status).toBe(200);
  });
  it("read-only hosts cannot ingest", async () => {
    expect((await postSpans(otlp(), READ)).status).toBe(403);
    expect((await postRaw(rawObject(), READ)).status).toBe(403);
  });
  it("fails closed for malformed or ambiguous host configuration", async () => {
    for (const hosts of ["", "{}", "not-json", '[{"token":"test-token-secret"}]', JSON.stringify([
      { token: TOKEN, tenant_id: "personal", device_id: "one", capabilities: ["read"] },
      { token: TOKEN, tenant_id: "other", device_id: "two", capabilities: ["read"] },
    ])]) {
      const response = await worker.fetch(new Request("https://x/v1/query?tenant_id=personal&op=traces", {
        headers: headers(),
      }), { DB: env.DB, GENTLY_HOSTS: hosts } as Env);
      expect(response.status).toBe(401);
    }
  });
  it("binds missing namespace attributes to the authenticated capture host", async () => {
    expect((await postSpans(otlp())).status).toBe(200);
    const response = await SELF.fetch("https://x/v1/query?tenant_id=personal&op=spans", { headers: headers() });
    const rows = await response.json<Array<{ resource_json: string }>>();
    const attrs = JSON.parse(rows[0].resource_json);
    expect(attrs).toContainEqual({ key: "gently.tenant_id", value: { stringValue: "personal" } });
    expect(attrs).toContainEqual({ key: "gently.device_id", value: { stringValue: "mac-main" } });
  });
  it("rejects conflicting or duplicate host claims in spans and resources", async () => {
    for (const attributes of [
      [{ key: "gently.device_id", value: { stringValue: "forged" } }],
      [{ key: "gently.tenant_id", value: { stringValue: "other" } }],
      [
        { key: "gently.device_id", value: { stringValue: "mac-main" } },
        { key: "gently.device_id", value: { stringValue: "mac-main" } },
      ],
    ]) {
      expect((await postSpans(otlp(attributes))).status).toBe(403);
      expect((await postSpans(otlp([], attributes))).status).toBe(403);
    }
  });
  it("keeps span ownership immutable while permitting cross-host parent links", async () => {
    expect((await postSpans(otlp())).status).toBe(200);
    expect((await postSpans(otlp(), INGEST)).status).toBe(409);
    const child = otlp();
    Object.assign(child.resourceSpans[0].scopeSpans[0].spans[0], {
      spanId: "33".repeat(8), parentSpanId: "22".repeat(8), name: "tool:child",
    });
    expect((await postSpans(child, INGEST)).status).toBe(200);
    const response = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=trace&trace_id=${"11".repeat(16)}`, { headers: headers() });
    const rows = await response.json<Array<{ span_id: string; parent_span_id: string | null; resource_json: string }>>();
    expect(rows).toHaveLength(2);
    expect(rows.find(row => row.span_id === "22".repeat(8))?.resource_json).toContain("mac-main");
    expect(rows.find(row => row.span_id === "33".repeat(8))?.parent_span_id).toBe("22".repeat(8));
  });
  it("accepts repeated logical reports but rejects conflicting trace ownership", async () => {
    const first = otlp();
    const repeated = otlp();
    expect((await postSpans({ resourceSpans: [...first.resourceSpans, ...repeated.resourceSpans] })).status).toBe(200);
    const conflicting = otlp();
    conflicting.resourceSpans[0].scopeSpans[0].spans[0].traceId = "44".repeat(16);
    expect((await postSpans({ resourceSpans: [...first.resourceSpans, ...conflicting.resourceSpans] })).status).toBe(409);
    expect((await postSpans(conflicting)).status).toBe(409);
  });
  it("bounds span count before database writes and permits a full bounded batch", async () => {
    const reports = Array.from({ length: 33 }, (_, index) => {
      const report = otlp();
      report.resourceSpans[0].scopeSpans[0].spans[0].spanId = (index + 1).toString(16).padStart(16, "0");
      return report;
    });
    expect((await postSpans({ resourceSpans: reports.flatMap(report => report.resourceSpans) })).status).toBe(413);
    expect(await env.DB.prepare("SELECT COUNT(*) AS n FROM spans WHERE tenant_id = ?").bind("personal").first<number>("n")).toBe(0);
    expect((await postSpans({ resourceSpans: reports.slice(0, 32).flatMap(report => report.resourceSpans) })).status).toBe(200);
    expect(await env.DB.prepare("SELECT COUNT(*) AS n FROM spans WHERE tenant_id = ?").bind("personal").first<number>("n")).toBe(32);
  });
  it("isolates every query and span upsert when tenants reuse IDs", async () => {
    const own = otlp();
    const other = otlp();
    other.resourceSpans[0].scopeSpans[0].spans[0].name = "tool:other-tenant";
    expect((await postSpans(own)).status).toBe(200);
    expect((await postSpans(other, OTHER, "other")).status).toBe(200);
    for (const op of ["traces", `trace&trace_id=${"11".repeat(16)}`, "spans", "stats"]) {
      const response = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=${op}`, { headers: headers() });
      const rows = await response.json<Array<Record<string, unknown>>>();
      expect(response.status).toBe(200);
      expect(rows.length).toBe(1);
      expect(JSON.stringify(rows)).not.toContain("other-tenant");
      if (op === "traces" || op === "stats") expect(rows[0].span_count).toBe(1);
    }
    const count = await env.DB.prepare("SELECT COUNT(*) AS n FROM spans").first<{ n: number }>();
    expect(count?.n).toBe(2);
  });
});

describe("opaque encrypted raw objects", () => {
  it("stores and fetches exactly the ciphertext envelope", async () => {
    expect((await postRaw()).status).toBe(200);
    const response = await getRaw();
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual(rawObject());
    expect(response.headers.get("Cache-Control")).toBe("no-store");
  });
  it("permits identical retry but refuses replacing an immutable reference", async () => {
    expect((await postRaw()).status).toBe(200);
    expect((await postRaw()).status).toBe(200);
    expect((await postRaw(rawObject({ ciphertext_b64: btoa(atob(CIPHERTEXT) + "changed") }))).status).toBe(409);
    expect(await (await getRaw()).json()).toEqual(rawObject());
  });
  it("returns not found for a reference absent from the requesting tenant", async () => {
    await postRaw();
    expect((await getRaw(RAW_REF, OTHER, "other")).status).toBe(404);
    expect((await getRaw("b2".repeat(16))).status).toBe(404);
  });
  it("derives object tenant and capture device from the credential", async () => {
    const original = rawObject();
    expect((await postRaw(rawObject({ context: { ...original.context, tenant_id: "other" } }))).status).toBe(403);
    expect((await postRaw(rawObject({ context: { ...original.context, device_id: "forged-device" } }))).status).toBe(403);
  });
  it("rejects plaintext and extra fields at both envelope levels", async () => {
    const original = rawObject();
    for (const object of [rawObject({ value: "synthetic-private-plaintext" }), rawObject({
      context: { ...original.context, prompt: "synthetic-private-plaintext" },
    }), rawObject({ ciphertext_b64: btoa("synthetic-private-plaintext") })]) {
      expect((await postRaw(object)).status).toBe(400);
    }
  });
  it("rejects invalid refs, unsupported versions, malformed base64 and epochs", async () => {
    const original = rawObject();
    for (const object of [rawObject({ version: 2 }), rawObject({ ciphertext_b64: "not-base64!" }),
      rawObject({ ciphertext_b64: btoa(atob(CIPHERTEXT) + "x").replace(/=+$/, "") }),
      rawObject({ context: { ...original.context, raw_ref: "old-sha-ref" } }),
      rawObject({ context: { ...original.context, key_epoch: 0 } }),
      rawObject({ context: { ...original.context, key_epoch: Number.MAX_SAFE_INTEGER + 1 } })]) {
      expect((await postRaw(object)).status).toBe(400);
    }
  });
  it("rejects prefix-only ciphertext, incomplete headers and password recipients", async () => {
    const valid = atob(CIPHERTEXT);
    const footer = valid.indexOf("\n--- ") + 1;
    const payload = valid.indexOf("\n", footer) + 1;
    for (const bytes of [
      "age-encryption.org/v1\nsynthetic-private-plaintext",
      "age-encryption.org/v1\n" + valid.slice(footer),
      valid.slice(0, footer + 10),
      valid.slice(0, payload + 16),
      valid.replace("-> X25519 ", "-> scrypt "),
      valid.replace(/(-> X25519 )[^\n]+/, "$1bad-share"),
      valid.replace(/(--- )[^\n]+/, "$1invalid-mac"),
    ]) {
      expect((await postRaw(rawObject({ ciphertext_b64: btoa(bytes) }))).status).toBe(400);
    }
  });
  it("bounds UTF-8 context bytes and rejects control characters", async () => {
    const original = rawObject();
    for (const session_id of ["é".repeat(129), "session\nprivate", "session\u0085private"]) {
      expect((await postRaw(rawObject({ context: { ...original.context, session_id } }))).status).toBe(400);
    }
  });
  it("bounds decoded ciphertext below the D1 row limit", async () => {
    const ciphertext_b64 = btoa("age-encryption.org/v1\n" + "x".repeat(512 * 1024));
    expect((await postRaw(rawObject({ ciphertext_b64 }))).status).toBe(413);
  });
  it("bounds actual streamed request bytes without relying on Content-Length", async () => {
    const chunk = new TextEncoder().encode("x".repeat(600 * 1024));
    const body = new ReadableStream<Uint8Array>({ start(controller) {
      controller.enqueue(chunk); controller.enqueue(chunk); controller.close();
    } });
    const response = await worker.fetch(new Request("https://x/v1/raw-values?tenant_id=personal", {
      method: "POST", headers: headers(), body,
    }), env as Env);
    expect(response.status).toBe(413);
  });
  it("rejects malformed JSON with a safe client error", async () => {
    const response = await SELF.fetch("https://x/v1/raw-values?tenant_id=personal", {
      method: "POST", headers: headers(), body: "{broken-json",
    });
    expect(response.status).toBe(400);
    expect(await response.text()).not.toContain("broken-json");
  });
});

describe("metadata ingest plaintext guards", () => {
  it("rejects known plaintext raw attributes in spans and resources", async () => {
    for (const key of ["gently.prompt", "gently.tool_input", "gently.tool_response", "gently.user", "gently.assistant", "gently.input", "gently.output", "gently.compact_instructions", "gently.compact_summary", "gently.tool_calls", "gently.error", "gently.error_details", "prompt", "tool_input", "assistant_message", "hook_payload", "gently.hook_payload", "message.delta", "gently.message.delta", "instruction_file", "gently.instruction_file"]) {
      const attrs = [{ key, value: { stringValue: "synthetic-private-plaintext" } }];
      expect((await postSpans(otlp(attrs))).status).toBe(400);
      expect((await postSpans(otlp([], attrs))).status).toBe(400);
    }
  });
  it("permits safe lifecycle reason enums but rejects arbitrary reason text", async () => {
    expect((await postSpans(otlp([{ key: "gently.reason", value: { stringValue: "shutdown" } }]))).status).toBe(200);
    expect((await postSpans(otlp([{ key: "gently.reason", value: { stringValue: "synthetic-private-plaintext" } }]))).status).toBe(400);
  });
  it("rejects public content fingerprints from any attribute", async () => {
    for (const key of ["gently.prompt.sha256", "gently.reason.sha256", "custom.sha256"]) {
      const attributes = [{ key, value: { stringValue: "abc123" } }];
      expect((await postSpans(otlp(attributes))).status).toBe(400);
    }
  });
  it("returns explicit span fields without internal storage columns", async () => {
    await postSpans(otlp());
    for (const op of ["spans", `trace&trace_id=${"11".repeat(16)}`]) {
      const response = await SELF.fetch(`https://x/v1/query?tenant_id=personal&op=${op}`, { headers: headers() });
      const rows = await response.json<Array<Record<string, unknown>>>();
      expect(rows).toHaveLength(1);
      expect(rows[0]).not.toHaveProperty("tenant_id");
      expect(rows[0]).not.toHaveProperty("ciphertext_b64");
      expect(rows[0]).not.toHaveProperty("envelope_json");
    }
  });
  it("accepts opaque raw references as metadata", async () => {
    const attributes = [{ key: "gently.prompt.raw_ref", value: { stringValue: RAW_REF } }];
    expect((await postSpans(otlp(attributes))).status).toBe(200);
  });
  it("rejects unsupported top-level plaintext properties", async () => {
    expect((await postSpans({ ...otlp(), prompt: "synthetic-private-plaintext" })).status).toBe(400);
  });
});
