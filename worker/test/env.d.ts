import type { Env as WorkerEnv } from "../src/d1";

declare global {
  namespace Cloudflare {
    interface Env extends WorkerEnv {}
  }
}
