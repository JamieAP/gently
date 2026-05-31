import { defineWorkersConfig } from "@cloudflare/vitest-pool-workers/config";

export default defineWorkersConfig({
  test: {
    poolOptions: {
      workers: {
        wrangler: { configPath: "./wrangler.toml" },
        miniflare: {
          compatibilityDate: "2025-01-01",
          d1Databases: ["DB"],
          bindings: { GENTLY_TOKEN: "test-token-secret" },
        },
      },
    },
  },
});
