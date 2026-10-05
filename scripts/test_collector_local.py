"""Isolated launcher contract checks. No vault or native auth-store access."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
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
        self.env["FIXTURE_PUBLIC_SETUP"] = json.dumps({
            "collector_url": "http://127.0.0.1:8787", "state_dir": str(self.home / ".gently"),
            "tenant_id": "personal", "device_id": "fixture-host",
        })
        self.env["GENTLY_STATE_DIR"] = str(self.home / ".gently")
        self.executable("gently", 'import os, sys, time\nfrom pathlib import Path\n'
            'assert sys.argv[1:] == ["export", "--watch"]\n'
            'assert os.environ.get("GENTLY_TOKEN") == "synthetic-fixture-only"\n'
            'assert os.environ["GENTLY_COLLECTOR_URL"] == "http://127.0.0.1:8787"\n'
            'setup = __import__("json").loads(os.environ["FIXTURE_PUBLIC_SETUP"])\n'
            'assert os.environ["GENTLY_TENANT_ID"] == setup["tenant_id"]\n'
            'assert os.environ["GENTLY_DEVICE_ID"] == setup["device_id"]\n'
            'assert os.environ["GENTLY_STATE_DIR"] == setup["state_dir"]\n'
            'Path(os.environ["FIXTURE_DIR"], "watch.pid").write_text(str(os.getpid()))\n'
            'time.sleep(60)\n')
        self.executable("node", 'import json, os, sys, time\nfrom pathlib import Path\n'
            'assert os.environ["WRANGLER_WRITE_LOGS"] == "false"\n'
            'assert os.environ["WRANGLER_SEND_METRICS"] == "false"\n'
            'assert os.environ["WRANGLER_LOG"] == "log"\n'
            'assert "GENTLY_TOKEN" not in os.environ\n'
            'hosts = json.loads(os.environ["GENTLY_HOSTS"])\n'
            'setup = json.loads(os.environ["FIXTURE_PUBLIC_SETUP"])\n'
            'assert hosts == [{"token": "synthetic-fixture-only", "tenant_id": setup["tenant_id"], "device_id": setup["device_id"], "capabilities": ["ingest", "read"]}]\n'
            'assert Path.cwd().resolve() == Path(sys.argv[1]).parents[3].resolve()\n'
            'assert sys.argv[2:4] == ["dev", "--config"]\n'
            'assert sys.argv[5:] == ["--local", "--ip", "127.0.0.1", "--port", "8787", "--env-file", "/dev/null"]\n'
            'Path(os.environ["FIXTURE_DIR"], "collector.pid").write_text(str(os.getpid()))\n'
            'time.sleep(0.3)\n')

    def executable(self, name, source):
        path = self.bin / name
        if name == "gently":
            source = ('import os,sys\n'
                'if sys.argv[1:] == ["config", "--json"]:\n'
                '    assert "GENTLY_TOKEN" not in os.environ\n'
                '    if os.environ.get("FIXTURE_CONFIG_INVALID"): raise SystemExit(1)\n'
                '    print(os.environ["FIXTURE_PUBLIC_SETUP"])\n'
                '    raise SystemExit(0)\n'
                'if sys.argv[1:] == ["config", "--check"]:\n'
                '    assert "GENTLY_TOKEN" not in os.environ\n'
                '    __import__("pathlib").Path(os.environ["FIXTURE_DIR"],"checked").touch()\n'
                '    raise SystemExit(1 if os.environ.get("FIXTURE_CHECK_INVALID") else 0)\n') + source
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
        for _ in range(100):
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.01)
        self.fail(f"synthetic child {name} remains running")

    def cleanup_pid(self, name):
        path = self.root / name
        if path.exists():
            try:
                os.kill(int(path.read_text()), 9)
            except ProcessLookupError:
                pass

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
        self.executable("gently", 'import os,time\nfrom pathlib import Path\n'
            'while not Path(os.environ["FIXTURE_DIR"],"collector.pid").exists(): time.sleep(0.01)\n'
            'raise SystemExit(7)\n')
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
        self.assertTrue((self.root / "checked").exists())

    def test_attach_launcher_runs_only_exporter(self):
        self.executable("gently", 'import os, sys\n'
            'assert sys.argv[1:] == ["export", "--watch"]\n'
            'assert os.environ.get("GENTLY_TOKEN") == "synthetic-fixture-only"\n')
        self.launch(self.repo / "scripts/export-local")
        self.assertFalse((self.root / "collector.pid").exists())

    def test_attach_preflight_checks_config_without_credentials(self):
        self.env.pop("GENTLY_TOKEN")
        result = subprocess.run([str(self.repo / "scripts/export-local"), "--check"],
            cwd=self.root, env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root / "checked").exists())
        self.assertFalse((self.root / "watch.pid").exists())

    def test_launch_uses_resolved_config_namespace_and_state(self):
        self.env.pop("GENTLY_TENANT_ID")
        self.env.pop("GENTLY_DEVICE_ID")
        self.env["FIXTURE_PUBLIC_SETUP"] = json.dumps({
            "collector_url": "https://fixture.invalid", "state_dir": str(self.root / "custom state"),
            "tenant_id": "configured-tenant", "device_id": "configured-device",
        })
        self.launch(self.repo / "scripts/collector-local")
        self.assert_stopped("watch.pid")

    def test_invalid_config_starts_no_services(self):
        self.env["FIXTURE_CONFIG_INVALID"] = "1"
        self.launch(self.repo / "scripts/collector-local", 1)
        self.assertFalse((self.root / "watch.pid").exists())
        self.assertFalse((self.root / "collector.pid").exists())

    def test_invalid_capture_policy_fails_preflight_without_credentials(self):
        self.env.pop("GENTLY_TOKEN")
        self.env["FIXTURE_CHECK_INVALID"] = "1"
        result = subprocess.run([str(self.repo / "scripts/collector-local"), "--check"],
            cwd=self.root, env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 1)
        self.assertFalse((self.root / "watch.pid").exists())
        self.assertFalse((self.root / "collector.pid").exists())

    def descendant_node(self, parent_source):
        child = ('import os,signal,time;from pathlib import Path;'
            'signal.signal(signal.SIGTERM,signal.SIG_IGN);'
            'Path(os.environ["FIXTURE_DIR"],"descendant.pid").write_text(str(os.getpid()));'
            'time.sleep(60)')
        self.addCleanup(self.cleanup_pid, "descendant.pid")
        self.executable("node", 'import os,signal,subprocess,sys,time\nfrom pathlib import Path\n'
            f'subprocess.Popen([sys.executable,"-c",{child!r}],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)\n'
            'while not Path(os.environ["FIXTURE_DIR"],"descendant.pid").exists(): time.sleep(0.01)\n'
            + parent_source)

    def test_exited_collector_parent_does_not_leave_a_descendant(self):
        self.descendant_node('raise SystemExit(0)\n')
        self.launch(self.repo / "scripts/collector-local")
        self.assert_stopped("descendant.pid")

    def test_stubborn_descendant_is_killed_with_failed_companion(self):
        self.executable("gently", 'import os,time\nfrom pathlib import Path\n'
            'while not Path(os.environ["FIXTURE_DIR"],"descendant.pid").exists(): time.sleep(0.01)\n'
            'raise SystemExit(7)\n')
        self.descendant_node('signal.signal(signal.SIGTERM,signal.SIG_IGN)\ntime.sleep(60)\n')
        self.launch(self.repo / "scripts/collector-local", 7)
        self.assert_stopped("descendant.pid")

    def test_interrupt_stops_both_services_cleanly(self):
        self.executable("node", 'import os,time\nfrom pathlib import Path\n'
            'Path(os.environ["FIXTURE_DIR"],"collector.pid").write_text(str(os.getpid()))\n'
            'time.sleep(60)\n')
        process = subprocess.Popen([str(self.repo / "scripts/collector-local")],
            cwd=self.root, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.addCleanup(lambda: process.kill() if process.poll() is None else None)
        deadline = time.monotonic() + 5
        while not all((self.root / name).exists() for name in ["watch.pid", "collector.pid"]):
            self.assertLess(time.monotonic(), deadline, "synthetic services did not start")
            time.sleep(0.01)
        process.send_signal(__import__("signal").SIGINT)
        stdout, stderr = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 0, stderr)
        self.assertNotIn(b"synthetic-fixture-only", stdout + stderr)
        self.assert_stopped("watch.pid")
        self.assert_stopped("collector.pid")


if __name__ == "__main__":
    unittest.main()
