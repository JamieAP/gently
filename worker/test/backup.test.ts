import { env } from "cloudflare:test";
import type { WorkflowStep } from "cloudflare:workers";
import { beforeEach, expect, it, vi } from "vitest";
import backup, { runBackup, type BackupEnv } from "../src/backup";

const bucket = env.BACKUP_TEST_BUCKET;
const configured: BackupEnv = {
  ACCOUNT_ID: "a".repeat(32), DATABASE_ID: "00000000-0000-0000-0000-000000000001",
  D1_REST_API_TOKEN: "synthetic-backup-token", BACKUP_BUCKET: bucket,
};
const event = { instanceId: "synthetic-run", timestamp: new Date("2026-01-01T03:00:00Z") };
const key = "d1-backups/v1/2026-01-01/synthetic-run.sql";
const sql = "CREATE TABLE synthetic_fixture (id INTEGER);\n";
const signedUrl = "https://fixture.r2.cloudflarestorage.com/export.sql?signature=synthetic-private-url";
let outputs: unknown[];
let savedBookmark: string | undefined;
const steps = () => ({ async do(name: string, _options: unknown, callback: () => Promise<unknown>) {
  if (name === "Start D1 export" && savedBookmark) return savedBookmark;
  const value = await callback();
  if (name === "Start D1 export") savedBookmark = value as string;
  outputs.push(value);
  return value;
} }) as unknown as WorkflowStep;
const started = () => Response.json({ success: true, result: { at_bookmark: "synthetic-bookmark" } });
const completed = (url = signedUrl) => Response.json({ success: true, result: {
  status: "complete", result: { signed_url: url },
} });
const download = () => new Response(sql, { headers: { "Content-Length": String(new TextEncoder().encode(sql).length) } });

beforeEach(async () => {
  outputs = [];
  savedBookmark = undefined;
  const objects = await bucket.list();
  if (objects.objects.length) await bucket.delete(objects.objects.map(o => o.key));
});

it("streams a full SQL export into private R2 without leaking the API token or signed URL", async () => {
  const fetcher = vi.fn<typeof fetch>().mockResolvedValueOnce(started()).mockResolvedValueOnce(completed()).mockResolvedValueOnce(download());
  expect(await runBackup(configured, event, steps(), fetcher)).toEqual({ key, bytes: sql.length });
  expect(await (await bucket.get(key))!.text()).toBe(sql);
  const marker = await (await bucket.get(`${key}.complete.json`))!.json<{bytes: number}>();
  expect(marker.bytes).toBe(sql.length);
  expect(JSON.parse(fetcher.mock.calls[0][1]!.body as string)).toEqual({ output_format: "polling" });
  expect(JSON.parse(fetcher.mock.calls[1][1]!.body as string)).toEqual({ output_format: "polling", current_bookmark: "synthetic-bookmark" });
  expect(fetcher.mock.calls[2]).toEqual([signedUrl, { redirect: "error" }]);
  expect(JSON.stringify(outputs)).not.toContain("synthetic-private-url");
  expect(JSON.stringify(outputs)).not.toContain(configured.D1_REST_API_TOKEN);
});

it("retries pending exports from the saved bookmark without starting another export", async () => {
  const fetcher = vi.fn<typeof fetch>().mockResolvedValueOnce(started())
    .mockResolvedValueOnce(Response.json({ success: true, result: {} }))
    .mockResolvedValueOnce(completed()).mockResolvedValueOnce(download());
  await expect(runBackup(configured, event, steps(), fetcher)).rejects.toThrow("D1 backup not confirmed");
  expect(await bucket.get(`${key}.complete.json`)).toBeNull();
  expect(await runBackup(configured, event, steps(), fetcher)).toEqual({ key, bytes: sql.length });
  expect(fetcher.mock.calls.filter(([, options]) => options?.body && !String(options.body).includes("current_bookmark"))).toHaveLength(1);
});

it("does not download or replace an already completed backup after a checkpoint retry", async () => {
  const fetcher = vi.fn<typeof fetch>().mockResolvedValueOnce(started()).mockResolvedValueOnce(completed()).mockResolvedValueOnce(download());
  await runBackup(configured, event, steps(), fetcher);
  const etag = (await bucket.head(key))!.etag;
  await runBackup(configured, event, steps(), fetcher);
  expect(fetcher).toHaveBeenCalledTimes(3);
  expect((await bucket.head(key))!.etag).toBe(etag);
});

it("does not mark a truncated download complete and never exposes upstream error text", async () => {
  const fetcher = vi.fn<typeof fetch>().mockResolvedValueOnce(started()).mockResolvedValueOnce(completed())
    .mockResolvedValueOnce(new Response(sql, { headers: { "Content-Length": String(sql.length + 1) } }));
  await expect(runBackup(configured, event, steps(), fetcher)).rejects.toThrow("D1 backup not confirmed");
  expect(await bucket.get(`${key}.complete.json`)).toBeNull();
  fetcher.mockRejectedValueOnce(new Error(`${signedUrl} ${configured.D1_REST_API_TOKEN}`));
  await expect(runBackup(configured, event, steps(), fetcher)).rejects.toThrow(/^D1 backup not confirmed; retry required$/);
});

it("rejects unsuccessful exports and unsafe download URLs", async () => {
  const failed = vi.fn<typeof fetch>().mockResolvedValueOnce(Response.json({ success: false, errors: [{ message: "private upstream error" }] }));
  await expect(runBackup(configured, event, steps(), failed)).rejects.toThrow(/^D1 export did not succeed$/);
  for (const url of ["http://fixture.invalid/backup", "https://user:password@fixture.invalid/backup"]) {
    savedBookmark = undefined;
    const fetcher = vi.fn<typeof fetch>().mockResolvedValueOnce(started()).mockResolvedValueOnce(completed(url));
    await expect(runBackup(configured, event, steps(), fetcher)).rejects.toThrow("D1 backup not confirmed");
    expect(fetcher).toHaveBeenCalledTimes(2);
  }
});

it("has no public backup trigger or credentials in its HTTP response", async () => {
  expect((await backup.fetch()).status).toBe(404);
  const fetcher = vi.fn<typeof fetch>();
  await expect(runBackup({ ...configured, ACCOUNT_ID: "invalid" }, event, steps(), fetcher)).rejects.toThrow("Invalid D1 backup configuration");
  expect(fetcher).not.toHaveBeenCalled();
});
