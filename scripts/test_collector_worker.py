"""Exercise the Node launcher against a synthetic Wrangler API contract."""
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import unittest


@unittest.skipUnless(shutil.which("node"), "Node.js is required")
class WorkerLauncherTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="gently-worker-launcher-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        worker = self.root / "worker"
        (worker / "scripts").mkdir(parents=True)
        self.script = worker / "scripts/collector-local.mjs"
        shutil.copy2(Path(__file__).resolve().parents[1] / "worker/scripts/collector-local.mjs", self.script)
        module = worker / "node_modules/wrangler"
        module.mkdir(parents=True)
        (module / "package.json").write_text('{"type":"module","exports":"./index.mjs"}')
        (module / "index.mjs").write_text('''
import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { EventEmitter } from "node:events";
export class unstable_DevEnv extends EventEmitter {
async startWorker(options) {
  assert.equal(options.config, resolve("wrangler.local.toml"));
  assert.deepEqual(options.envFiles, ["/dev/null"]);
  assert.equal(options.sendMetrics, false);
  assert.deepEqual(options.dev, {
    remote: false, inspector: false,
    server: { hostname: "127.0.0.1", port: 8787 },
    persist: resolve(".wrangler/state"), liveReload: false,
    enableContainers: false, generateTypes: false, logLevel: "none",
  });
  if (process.env.FIXTURE_FAILURE === "startup") throw new Error("synthetic-private-value");
  this.keepAlive = setInterval(() => {}, 100);
  writeFileSync("../ready", "ready");
  if (process.env.FIXTURE_FAILURE === "async") setTimeout(() => this.emit("error", new Error("synthetic-private-value")), 50);
  if (process.env.FIXTURE_FAILURE === "running") setTimeout(() => this.emit("error", new Error("synthetic-private-value")), 100);
  return {
    ready: ["pending", "async"].includes(process.env.FIXTURE_FAILURE) ? new Promise(() => {}) : Promise.resolve(),
    inspectorUrl: Promise.resolve(process.env.FIXTURE_FAILURE === "inspector" ? new URL("http://127.0.0.1:9229") : undefined),
  };
}
    async teardown() {
      clearInterval(this.keepAlive);
      writeFileSync("../disposed", "disposed");
      if (process.env.FIXTURE_FAILURE === "cleanup") throw new Error("synthetic-private-value");
    }
}
''')

    def run_worker(self, failure=None):
        environment = {"PATH": os.environ["PATH"]}
        if failure:
            environment["FIXTURE_FAILURE"] = failure
        process = subprocess.Popen([shutil.which("node"), str(self.script)],
            cwd=self.root / "worker", env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.addCleanup(lambda: process.kill() if process.poll() is None else None)
        if failure not in {"startup", "inspector", "async", "running"}:
            deadline = time.monotonic() + 5
            while not (self.root / "ready").exists():
                self.assertIsNone(process.poll(), "Worker failed before readiness")
                self.assertLess(time.monotonic(), deadline)
                time.sleep(0.01)
            process.send_signal(signal.SIGTERM)
        output, errors = process.communicate(timeout=10)
        self.assertNotIn(b"synthetic-private-value", output + errors)
        return process.returncode, errors

    def test_disables_inspector_and_remote_services_and_disposes_on_signal(self):
        code, errors = self.run_worker()
        self.assertEqual(code, 0, errors)
        self.assertTrue((self.root / "disposed").exists())

    def test_unexpected_inspector_fails_closed_and_disposes(self):
        code, errors = self.run_worker("inspector")
        self.assertEqual(code, 1, errors)
        self.assertTrue((self.root / "disposed").exists())

    def test_startup_failure_withholds_dependency_error(self):
        code, errors = self.run_worker("startup")
        self.assertEqual(code, 1)
        self.assertIn(b"diagnostic values withheld", errors)

    def test_cleanup_failure_withholds_dependency_error(self):
        code, errors = self.run_worker("cleanup")
        self.assertEqual(code, 1)
        self.assertIn(b"cleanup failed", errors)

    def test_signal_while_readiness_pending_still_cleans_up(self):
        code, errors = self.run_worker("pending")
        self.assertEqual(code, 0, errors)
        self.assertTrue((self.root / "disposed").exists())

    def test_asynchronous_startup_error_cleans_up_without_readiness(self):
        code, errors = self.run_worker("async")
        self.assertEqual(code, 1, errors)
        self.assertTrue((self.root / "disposed").exists())

    def test_asynchronous_running_error_exits_and_cleans_up(self):
        code, errors = self.run_worker("running")
        self.assertEqual(code, 1, errors)
        self.assertTrue((self.root / "disposed").exists())


if __name__ == "__main__":
    unittest.main()
