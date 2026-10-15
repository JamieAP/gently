import { env } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import worker from "../src/index";

const token = "synthetic-security-token";
const host = (extra = {}) => ({ token, tenant_id: "personal", device_id: "fixture", capabilities: ["read"], ...extra });
const request = (url = "https://fixture/v1/whoami?tenant_id=personal", credential = token) =>
  new Request(url, { headers: { Authorization: `Bearer ${credential}`, "CF-Connecting-IP": "192.0.2.1" } });
const configured = (extra = {}) => ({ ...env, GENTLY_HOSTS: JSON.stringify([host()]), ...extra });

it("rejects public HTTP before authentication or database work, without redirecting credentials", async () => {
  for (const credential of [token, "invalid"]) {
    const response = await worker.fetch(request("http://fixture/v1/whoami?tenant_id=personal", credential), configured());
    expect(response.status).toBe(403);
    expect(response.headers.has("Location")).toBe(false);
  }
  expect((await worker.fetch(request("http://127.0.0.1:8787/v1/whoami?tenant_id=personal"), configured())).status).toBe(200);
});

it("rejects expired and malformed expiry credentials but permits unexpired rotation overlap", async () => {
  for (const expiry of [0, -1, 1.5, "tomorrow", Math.floor(Date.now() / 1000) - 1]) {
    const response = await worker.fetch(request(), configured({ GENTLY_HOSTS: JSON.stringify([host({ expires_unix_secs: expiry })]) }));
    expect(response.status).toBe(401);
  }
  const hosts = [host({ expires_unix_secs: 1 }), host({ token: "new-fixture-token", expires_unix_secs: Math.floor(Date.now() / 1000) + 3600 })];
  expect((await worker.fetch(request(undefined, "new-fixture-token"), configured({ GENTLY_HOSTS: JSON.stringify(hosts) }))).status).toBe(200);
});

it("requires cloud expiry when configured, while allowing legacy local hosts", async () => {
  expect((await worker.fetch(request(), configured({ REQUIRE_CREDENTIAL_EXPIRY: "true" }))).status).toBe(401);
  expect((await worker.fetch(request(), configured())).status).toBe(200);
});

it("rate-limits invalid credentials without placing tokens in limiter keys or throttling valid credentials", async () => {
  const limit = vi.fn().mockResolvedValue({ success: false });
  const config = configured({ AUTH_RATE_LIMITER: { limit } });
  const response = await worker.fetch(request(undefined, "invalid-sensitive-token"), config);
  expect(response.status).toBe(429);
  expect(response.headers.get("Retry-After")).toBe("60");
  expect(JSON.stringify(limit.mock.calls)).not.toContain("invalid-sensitive-token");
  expect((await worker.fetch(request(), config)).status).toBe(200);
  expect(limit).toHaveBeenCalledTimes(1);
});

it("limits authenticated read and ingest separately and fails closed on limiter errors", async () => {
  const read = vi.fn().mockResolvedValue({ success: false });
  const ingest = vi.fn().mockResolvedValue({ success: true });
  const config = configured({ READ_RATE_LIMITER: { limit: read }, INGEST_RATE_LIMITER: { limit: ingest } });
  expect((await worker.fetch(request(), config)).status).toBe(429);
  expect(ingest).not.toHaveBeenCalled();
  expect(JSON.stringify(read.mock.calls)).not.toContain(token);
  read.mockRejectedValue(new Error("sensitive backend diagnostic"));
  const failed = await worker.fetch(request(), config);
  expect(failed.status).toBe(503);
  expect(await failed.text()).not.toContain("sensitive");
});

it("fails closed if required production limiters are missing", async () => {
  expect((await worker.fetch(request(), configured({ REQUIRE_RATE_LIMITS: "true" }))).status).toBe(503);
});

it("does not let one capability exhaust the other capability's rate bucket", async () => {
  const read = vi.fn().mockResolvedValue({ success: true });
  const ingest = vi.fn().mockResolvedValue({ success: true });
  const config = configured({ READ_RATE_LIMITER: { limit: read }, INGEST_RATE_LIMITER: { limit: ingest } });
  const write = new Request("https://fixture/v1/traces?tenant_id=personal", {
    method: "POST", headers: { Authorization: `Bearer ${token}` }, body: '{"resourceSpans":[]}',
  });
  expect((await worker.fetch(write, config)).status).toBe(403);
  expect(ingest).not.toHaveBeenCalled();
  const writer = { ...config, GENTLY_HOSTS: JSON.stringify([host({ capabilities: ["ingest"] })]) };
  expect((await worker.fetch(request("https://fixture/v1/query?tenant_id=personal&op=traces"), writer)).status).toBe(403);
  expect(read).not.toHaveBeenCalled();
});
