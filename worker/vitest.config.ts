import { defineWorkersConfig } from "@cloudflare/vitest-pool-workers/config";

export default defineWorkersConfig({
  test: {
    poolOptions: {
      workers: {
        wrangler: { configPath: "./wrangler.toml" },
        miniflare: {
          compatibilityDate: "2025-01-01",
          d1Databases: ["DB"],
          bindings: { GENTLY_HOSTS: JSON.stringify([
            { token: "test-token-secret", tenant_id: "personal", device_id: "mac-main", capabilities: ["ingest", "read"] },
            { token: "other-tenant-token", tenant_id: "other", device_id: "linux-other", capabilities: ["ingest", "read"] },
            { token: "ingest-only-token", tenant_id: "personal", device_id: "linux-capture", capabilities: ["ingest"] },
            { token: "read-only-token", tenant_id: "personal", device_id: "mac-reader", capabilities: ["read"] },
          ]) },
        },
      },
    },
  },
});
