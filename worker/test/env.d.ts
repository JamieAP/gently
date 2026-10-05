import type { Env } from "../src/d1";

declare module "cloudflare:test" {
  interface ProvidedEnv extends Env {}
}
