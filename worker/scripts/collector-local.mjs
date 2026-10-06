// Wrangler's CLI enables DevTools; its API can disable the inspector entirely.
import { unstable_DevEnv as DevEnv } from "wrangler";
import { fileURLToPath } from "node:url";

const runtime = new DevEnv();
let signalStopped;
const stopped = new Promise((resolve) => { signalStopped = () => resolve("stopped"); });
for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, signalStopped);
}
let signalFailure;
const failed = new Promise((resolve) => { signalFailure = () => resolve("failed"); });
runtime.on("error", signalFailure);
let startupTimer;
const timedOut = new Promise((resolve) => {
  startupTimer = setTimeout(() => resolve("failed"), 30_000);
});

try {
  const starting = runtime.startWorker({
    config: fileURLToPath(new URL("../wrangler.local.toml", import.meta.url)),
    envFiles: ["/dev/null"],
    sendMetrics: false,
    dev: {
      remote: false,
      inspector: false,
      server: { hostname: "127.0.0.1", port: 8787 },
      persist: fileURLToPath(new URL("../.wrangler/state", import.meta.url)),
      liveReload: false,
      enableContainers: false,
      generateTypes: false,
      logLevel: "none",
    },
  }).then(async (worker) => {
    await worker.ready;
    if (await worker.inspectorUrl !== undefined) {
      throw new Error("Inspector must be disabled");
    }
    return "ready";
  });
  const result = await Promise.race([starting, stopped, failed, timedOut]);
  clearTimeout(startupTimer);
  if (result === "failed" || result === "ready" && await Promise.race([stopped, failed]) === "failed") {
    throw new Error("Local Worker failed");
  }
} catch {
  // Dependency errors may contain binding values; report only a fixed message.
  console.error("Gently local Worker failed; diagnostic values withheld.");
  process.exitCode = 1;
} finally {
  clearTimeout(startupTimer);
  // Worker.dispose() waits for readiness, which need not settle on startup failure.
  // Teardown the underlying runtime instead, with a bound so the supervisor can
  // reap the entire process group even if a dependency's cleanup hangs.
  const cleanupTimer = setTimeout(() => {
    console.error("Gently local Worker cleanup timed out; diagnostic values withheld.");
    process.exit(1);
  }, 5_000);
  try {
    await runtime.teardown();
  } catch {
    console.error("Gently local Worker cleanup failed; diagnostic values withheld.");
    process.exitCode = 1;
  } finally {
    clearTimeout(cleanupTimer);
  }
}
