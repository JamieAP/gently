"""Isolated launcher contract checks. No vault or native auth-store access."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]


class LauncherTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="gently-launcher-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.repo = self.root / "repo with spaces"
        (self.repo / "scripts").mkdir(parents=True)
        for name in ("collector-local", "collector-supervise.py", "export-local"):
            shutil.copy2(REPO / "scripts" / name, self.repo / "scripts" / name)
        self.wrangler = self.repo / "worker/node_modules/wrangler/bin/wrangler.js"
        self.wrangler.parent.mkdir(parents=True)
        self.wrangler.touch()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "python3").symlink_to(sys.executable)
        self.home = self.root / "home"
        self.home.mkdir()
        self.env = {"HOME": str(self.home), "PATH": f"{self.bin}:/usr/bin:/bin",
                    "FIXTURE_DIR": str(self.root), "WRANGLER_WRITE_LOGS": "true",
                    "GENTLY_TOKEN": "synthetic-fixture-only",
                    "GENTLY_TENANT_ID": "personal", "GENTLY_DEVICE_ID": "fixture-host"}
        self.executable("gently", 'import os, sys, time\nfrom pathlib import Path\n'
            'assert sys.argv[1:] == ["export", "--watch"]\n'
            'assert os.environ.get("GENTLY_TOKEN") == "synthetic-fixture-only"\n'
            'assert os.environ["GENTLY_COLLECTOR_URL"] == "http://127.0.0.1:8787"\n'
            'Path(os.environ["FIXTURE_DIR"], "watch.pid").write_text(str(os.getpid()))\n'
            'time.sleep(60)\n')
        self.executable("node", 'import json, os, sys, time\nfrom pathlib import Path\n'
            'assert os.environ["WRANGLER_WRITE_LOGS"] == "false"\n'
            'assert os.environ["WRANGLER_SEND_METRICS"] == "false"\n'
            'assert os.environ["WRANGLER_LOG"] == "log"\n'
            'assert "GENTLY_TOKEN" not in os.environ\n'
            'hosts = json.loads(os.environ["GENTLY_HOSTS"])\n'
            'assert hosts == [{"token": "synthetic-fixture-only", "tenant_id": "personal", "device_id": "fixture-host", "capabilities": ["ingest", "read"]}]\n'
            'assert Path.cwd().resolve() == Path(sys.argv[1]).parents[3].resolve()\n'
            'assert sys.argv[2:4] == ["dev", "--config"]\n'
            'assert sys.argv[5:] == ["--local", "--ip", "127.0.0.1", "--port", "8787", "--env-file", "/dev/null"]\n'
            'Path(os.environ["FIXTURE_DIR"], "collector.pid").write_text(str(os.getpid()))\n'
            'time.sleep(0.3)\n')

    def executable(self, name, source):
        path = self.bin / name
        path.write_text(f"#!{sys.executable}\n{source}")
        path.chmod(0o755)
        return path

    def launch(self, path, code=0):
        result = subprocess.run([str(path)], cwd=self.root, env=self.env,
            capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, code, result.stderr)
        self.assertNotIn("synthetic-fixture-only", result.stdout + result.stderr)
        return result

    def assert_stopped(self, name):
        pid = int((self.root / name).read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)

    def test_direct_launch_supervises_both_children(self):
        self.launch(self.repo / "scripts/collector-local")
        self.assert_stopped("watch.pid")
        self.assert_stopped("collector.pid")

    def test_relative_symlink_chain_resolves_repository(self):
        first = self.bin / "gently-collector"
        first.symlink_to("../repo with spaces/scripts/collector-local")
        second = self.bin / "alias"
        second.symlink_to("gently-collector")
        self.launch(second)
        self.assert_stopped("watch.pid")

    def test_exporter_failure_stops_collector(self):
        self.executable("gently", 'import time\ntime.sleep(0.1)\nraise SystemExit(7)\n')
        self.executable("node", 'import os, time\nfrom pathlib import Path\n'
            'Path(os.environ["FIXTURE_DIR"], "collector.pid").write_text(str(os.getpid()))\n'
            'time.sleep(60)\n')
        self.launch(self.repo / "scripts/collector-local", 7)
        self.assert_stopped("collector.pid")

    def test_missing_dependency_never_unlocks(self):
        self.wrangler.unlink()
        result = self.launch(self.repo / "scripts/collector-local", 1)
        self.assertIn("Install collector dependencies", result.stderr)
        self.assertFalse((self.root / "unlocked").exists())

    def test_missing_token_is_provider_neutral(self):
        self.env.pop("GENTLY_TOKEN")
        result = self.launch(self.repo / "scripts/collector-local", 1)
        self.assertIn("GENTLY_TOKEN", result.stderr)
        self.assertNotIn("agent-secrets", result.stderr)
        self.assertFalse((self.root / "watch.pid").exists())

    def test_preflight_checks_dependencies_without_credentials_or_services(self):
        self.env.pop("GENTLY_TOKEN")
        result = subprocess.run([str(self.repo / "scripts/collector-local"), "--check"],
            cwd=self.root, env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / "watch.pid").exists())
        self.assertFalse((self.root / "collector.pid").exists())

    def test_attach_launcher_runs_only_exporter(self):
        self.executable("gently", 'import os, sys\n'
            'assert sys.argv[1:] == ["export", "--watch"]\n'
            'assert os.environ.get("GENTLY_TOKEN") == "synthetic-fixture-only"\n')
        self.launch(self.repo / "scripts/export-local")
        self.assertFalse((self.root / "collector.pid").exists())


if __name__ == "__main__":
    unittest.main()
