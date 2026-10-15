import { WorkflowEntrypoint, type WorkflowEvent, type WorkflowStep } from "cloudflare:workers";

export interface BackupEnv {
  ACCOUNT_ID: string;
  DATABASE_ID: string;
  D1_REST_API_TOKEN: string;
  BACKUP_BUCKET: R2Bucket;
}

interface ExportResult {
  success?: boolean;
  result?: {
    at_bookmark?: string;
    status?: string;
    result?: { signed_url?: string };
  };
}

const retry = { retries: { limit: 120, delay: "30 seconds" as const, backoff: "constant" as const }, timeout: "15 minutes" as const };

/** Kept separate from the collector: no public trigger and no D1 write binding. */
export class D1Backup extends WorkflowEntrypoint<BackupEnv> {
  async run(event: WorkflowEvent<unknown>, step: WorkflowStep): Promise<unknown> {
    return runBackup(this.env, event, step);
  }
}

export async function runBackup(
  env: BackupEnv, event: { instanceId: string; timestamp: Date }, step: WorkflowStep,
  fetcher: typeof fetch = fetch,
): Promise<{ key: string; bytes: number }> {
  if (!/^[0-9a-f]{32}$/.test(env.ACCOUNT_ID) ||
      !/^[0-9a-f-]{36}$/.test(env.DATABASE_ID) || !env.D1_REST_API_TOKEN ||
      !/^[A-Za-z0-9_-]{1,100}$/.test(event.instanceId) || !Number.isFinite(event.timestamp.getTime()))
    throw new Error("Invalid D1 backup configuration");
  const endpoint = `https://api.cloudflare.com/client/v4/accounts/${env.ACCOUNT_ID}/d1/database/${env.DATABASE_ID}/export`;
  const key = `d1-backups/v1/${event.timestamp.toISOString().slice(0, 10)}/${event.instanceId}.sql`;
  const marker = `${key}.complete.json`;

  async function exportRequest(body: object): Promise<NonNullable<ExportResult["result"]>> {
    let response: Response;
    let data: ExportResult;
    try {
      response = await fetcher(endpoint, {
        method: "POST", redirect: "error",
        headers: { Authorization: `Bearer ${env.D1_REST_API_TOKEN}`, "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      if (!response.ok) throw new Error();
      data = await response.json<ExportResult>();
    } catch { throw new Error("D1 export request failed"); }
    if (!data?.success || !data.result || data.result.status === "error")
      throw new Error("D1 export did not succeed");
    return data.result;
  }

  const bookmark = await step.do("Start D1 export", retry, async () => {
    const result = await exportRequest({ output_format: "polling" });
    if (!result.at_bookmark) throw new Error("D1 export bookmark unavailable");
    return result.at_bookmark;
  });

  return step.do("Save completed export to private R2", retry, async () => {
    try {
      // If the upload committed but the Workflow checkpoint did not, do not
      // overwrite that snapshot or start a different export under the same key.
      const completed = await env.BACKUP_BUCKET.get(marker);
      if (completed) {
        const record = await completed.json<{ key: string; bytes: number; etag: string }>();
        const object = await env.BACKUP_BUCKET.head(key);
        if (record.key !== key || !object || record.bytes !== object.size || record.etag !== object.etag)
          throw new Error();
        return { key, bytes: object.size };
      }
      const result = await exportRequest({ output_format: "polling", current_bookmark: bookmark });
      if (result.status !== "complete" || !result.result?.signed_url)
        throw new Error();
      const download = new URL(result.result.signed_url);
      if (download.protocol !== "https:" || download.username || download.password) throw new Error();
      // Never forward the API credential to the signed download URL. Keep that
      // URL inside this step: no logs or durable Workflow step outputs contain it.
      const response = await fetcher(download.toString(), { redirect: "error" });
      if (!response.ok || !response.body) throw new Error();
      const size = response.headers.get("Content-Length");
      const object = await env.BACKUP_BUCKET.put(key, response.body, {
        httpMetadata: { contentType: "application/sql", cacheControl: "no-store" },
      });
      const stored = await env.BACKUP_BUCKET.head(key);
      if (!object || !stored || stored.size === 0 || stored.etag !== object.etag ||
          (size !== null && (!/^\d+$/.test(size) || Number(size) !== stored.size))) throw new Error();
      await env.BACKUP_BUCKET.put(marker, JSON.stringify({ version: 1, key, bytes: stored.size, etag: stored.etag }), {
        httpMetadata: { contentType: "application/json", cacheControl: "no-store" },
      });
      return { key, bytes: stored.size };
    } catch {
      // SDK/network errors can contain credentials, signed URLs or body snippets.
      throw new Error("D1 backup not confirmed; retry required");
    }
  });
}

export default {
  async fetch(): Promise<Response> { return new Response("Not found", { status: 404 }); },
};
