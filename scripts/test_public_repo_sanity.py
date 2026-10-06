import os
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]


class PublicRepoSanityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = dict(os.environ)
        self.env.pop("GENTLY_PUBLIC_REPO_SANITY", None)
        self.env.pop("GENTLY_TOKEN", None)
        self.run_git("init", "-q")
        self.run_git("config", "user.name", "Synthetic reviewer")
        self.run_git("config", "user.email", "reviewer@example.invalid")
        shutil.copytree(REPO / ".githooks", self.root / ".githooks")
        (self.root / "scripts").mkdir()
        shutil.copyfile(REPO / "scripts/public-repo-sanity.py", self.root / "scripts/public-repo-sanity.py")
        for hook in (self.root / ".githooks").iterdir():
            hook.chmod(0o755)
        self.run_git("config", "core.hooksPath", ".githooks")

    def run_git(self, *args, ack=False, check=True):
        env = dict(self.env)
        if ack:
            env["GENTLY_PUBLIC_REPO_SANITY"] = "1"
        result = subprocess.run(["git", *args], cwd=self.root, env=env,
                                capture_output=True, text=True)
        if check:
            self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def stage(self, name, content):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        self.run_git("add", "--", name)

    def push_hook(self, oid, ack=True, old="0" * 40, remote="unknown", destination="https://example.invalid/public.git"):
        env = dict(self.env)
        if ack:
            env["GENTLY_PUBLIC_REPO_SANITY"] = "1"
        return subprocess.run([str(self.root / ".githooks/pre-push"), remote, destination],
                              cwd=self.root, env=env, input=f"refs/heads/main {oid} refs/heads/main {old}\n",
                              capture_output=True, text=True)

    def test_commit_requires_explicit_review_and_clean_index_passes(self):
        self.stage("hello.txt", "synthetic public content\n")
        denied = self.run_git("commit", "-qm", "clean", check=False)
        self.assertNotEqual(denied.returncode, 0)
        self.assertIn("GENTLY_PUBLIC_REPO_SANITY=1", denied.stderr)
        self.run_git("commit", "-qm", "clean", ack=True)

    def test_staged_secret_is_blocked_even_when_working_tree_is_clean(self):
        key = "gh" + "p_" + "A" * 36
        self.stage("config.txt", key + "\n")
        (self.root / "config.txt").write_text("clean working copy\n")
        result = self.run_git("commit", "-qm", "test", ack=True, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("possible GitHub token", result.stderr)
        self.assertNotIn(key, result.stderr + result.stdout)

    def test_private_artifacts_are_blocked_without_revealing_contents(self):
        for name in [".env", "capture.jsonl", "state.db", "state.sqlite-wal", "state.sqlite3-shm", "state.sqlite3-journal", ".gently/raw/event.json", ".claude/settings.json", "capture.log", "scripts/__pycache__/module.pyc", "reader.age", "owner.enc", "export.zip", "export.har", "state.backup", "traces/event.json", ".aws/credentials"]:
            with self.subTest(name=name):
                self.stage(name, "private synthetic content\n")
                result = self.run_git("commit", "-qm", "test", ack=True, check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("private artifact path", result.stderr)
                self.assertNotIn("private synthetic content", result.stderr)
                self.run_git("rm", "--cached", "--", name)

    def test_renamed_and_wrapped_captures_are_blocked(self):
        captures = [
            {"resourceSpans": []},
            {"resource_spans": []},
            {"hook_event_name": "SyntheticHook", "session_id": "fixture"},
            {"context": {}, "ciphertext_b64": "synthetic"},
            {"trace_id": "fixture", "span_id": "fixture"},
            {"traceId": "fixture", "spanId": "fixture"},
            {"type": "event_msg", "payload": {}},
            {"type": "assistant", "message": {}},
            {"access_token": "synthetic-credential-canary"},
            {"token": "synthetic-credential-canary", "tenant_id": "fixture", "device_id": "fixture"},
        ]
        for capture in captures:
            with self.subTest(shape=list(capture)):
                self.stage("notes.txt", json.dumps({"wrapper": [capture], "private": "synthetic-capture-canary"}))
                result = self.run_git("commit", "-qm", "fixture", ack=True, check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("synthetic-capture-canary", result.stderr)
                self.assertNotIn("synthetic-credential-canary", result.stderr)
                self.run_git("rm", "--cached", "notes.txt")

    def test_renamed_jsonl_capture_is_blocked(self):
        self.stage("notes.txt", json.dumps({"kind": "public fixture"}) + "\n" + json.dumps({"resourceSpans": []}) + "\n")
        result = self.run_git("commit", "-qm", "fixture", ack=True, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("captured agent or telemetry", result.stderr)

    def test_scalar_bom_and_heading_prefixes_cannot_hide_captures(self):
        capture = json.dumps({"resourceSpans": []})
        for text in ['null\n' + capture, '\ufeff' + capture, 'Public-looking heading\n' + capture]:
            with self.subTest(prefix=text[:5]):
                self.stage("notes.txt", text)
                result = self.run_git("commit", "-qm", "fixture", ack=True, check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("captured agent or telemetry", result.stderr)
                self.run_git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "unsafe synthetic fixture")
                self.run_git("rm", "notes.txt")
                self.run_git("commit", "-qm", "clean tip", ack=True)
                oid = self.run_git("rev-parse", "HEAD").stdout.strip()
                result = self.push_hook(oid)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("captured agent or telemetry", result.stderr)

    def test_long_public_source_file_does_not_hit_json_record_limit(self):
        self.stage("example.py", 'public_code = 1\n' * 5000)
        self.run_git("commit", "-qm", "public fixture", ack=True)

    def test_json_escaping_cannot_hide_secret_or_local_home(self):
        key = "gh" + "p_" + "A" * 36
        for value in [key, str(Path.home())]:
            encoded = ''.join('\\u%04x' % ord(char) for char in value)
            self.stage("notes.txt", '{"description":"' + encoded + '"}')
            result = self.run_git("commit", "-qm", "fixture", ack=True, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn(value, result.stderr)
            self.run_git("rm", "--cached", "notes.txt")

    def test_renamed_database_ciphertext_and_archive_magic_are_blocked(self):
        signatures = [b"SQLite format 3\0", b"age-encryption.org/v1\n", b"-----BEGIN AGE ENCRYPTED FILE-----", b"PK\x03\x04", b"\x1f\x8b", b"BZh", b"\xfd7zXZ\x00", b"7z\xbc\xaf\x27\x1c", b"Rar!\x1a\x07", b"\x28\xb5\x2f\xfd", b"\0" * 257 + b"ustar"]
        for signature in signatures:
            with self.subTest(signature=signature[:8]):
                (self.root / "notes.txt").write_bytes(signature + b"synthetic-private-canary")
                self.run_git("add", "notes.txt")
                result = self.run_git("commit", "-qm", "fixture", ack=True, check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("database, encrypted data or archive", result.stderr)
                self.assertNotIn("synthetic-private-canary", result.stderr)
                self.run_git("rm", "--cached", "notes.txt")

    def test_private_reader_identity_and_local_home_paths_are_blocked(self):
        for content in ["AGE-" + "PLUGIN-SE-1" + "A" * 40, str(Path.home() / "private-project/file.txt")]:
            self.stage("notes.txt", content)
            result = self.run_git("commit", "-qm", "fixture", ack=True, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn(content, result.stderr)
            self.run_git("rm", "--cached", "notes.txt")

    def test_public_source_literals_and_unrelated_json_pass(self):
        self.stage("example.py", 'fixture = {"resourceSpans": []}\n')
        self.stage("example.rs", 'const FIXTURE: &str = r#"\n{"trace_id":"fixture","span_id":"fixture"}\n"#;\n')
        self.stage("package.json", json.dumps({"name": "public-fixture", "type": ["synthetic"], "scripts": {"test": "python3"}}))
        self.run_git("commit", "-qm", "public fixtures", ack=True)

    def test_renaming_entire_jsonl_capture_as_source_does_not_bypass_scan(self):
        self.stage("example.rs", 'null\n' + json.dumps({"resourceSpans": []}) + '\n')
        result = self.run_git("commit", "-qm", "fixture", ack=True, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("captured agent or telemetry", result.stderr)

    def test_deleted_capture_is_still_blocked_from_push_history(self):
        self.stage("notes.txt", json.dumps({"wrapper": [{"resourceSpans": []}]}))
        self.run_git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "unsafe synthetic fixture")
        self.run_git("rm", "notes.txt")
        self.run_git("commit", "-qm", "clean tip", ack=True)
        oid = self.run_git("rev-parse", "HEAD").stdout.strip()
        result = self.push_hook(oid)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("captured agent or telemetry", result.stderr)

    def test_staged_whitespace_has_a_fixed_actionable_diagnostic(self):
        self.stage("notes.txt", "synthetic public content  \n")
        result = self.run_git("commit", "-qm", "fixture", ack=True, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("git diff --cached --check", result.stderr)
        self.assertNotIn("synthetic public content", result.stderr)

    def test_missing_remote_commit_requires_fetch_without_weakening_scan(self):
        self.stage("notes.txt", "synthetic public fixture\n")
        self.run_git("commit", "-qm", "fixture", ack=True)
        oid = self.run_git("rev-parse", "HEAD").stdout.strip()
        result = self.push_hook(oid, old="1" * 40)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("fetch that remote and retry", result.stderr)

    def test_clean_push_requires_review(self):
        self.stage("hello.txt", "synthetic content\n")
        self.run_git("commit", "-qm", "clean", ack=True)
        oid = self.run_git("rev-parse", "HEAD").stdout.strip()
        self.assertNotEqual(self.push_hook(oid, ack=False).returncode, 0)
        self.assertEqual(self.push_hook(oid).returncode, 0)

    def test_git_push_invokes_installed_gate(self):
        destination = self.root / "public-fixture.git"
        self.run_git("init", "--bare", "-q", str(destination))
        self.run_git("remote", "add", "public-fixture", str(destination))
        self.stage("hello.txt", "synthetic content\n")
        self.run_git("commit", "-qm", "clean", ack=True)
        result = self.run_git("push", "public-fixture", "HEAD:refs/heads/main", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("GENTLY_PUBLIC_REPO_SANITY=1", result.stderr)
        self.run_git("push", "public-fixture", "HEAD:refs/heads/main", ack=True)

    def test_push_scans_secret_deleted_from_the_final_tree(self):
        key = "sk-" + "ant-" + "A" * 36
        self.stage("config.txt", key + "\n")
        # Build unsafe history in a fixture, then restore the installed guard.
        self.run_git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "unsafe fixture")
        self.run_git("rm", "config.txt")
        self.run_git("commit", "-qm", "remove unsafe fixture", ack=True)
        oid = self.run_git("rev-parse", "HEAD").stdout.strip()
        result = self.push_hook(oid)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("possible Anthropic API key", result.stderr)
        self.assertNotIn(key, result.stderr + result.stdout)

    def test_push_scans_commit_and_annotated_tag_messages(self):
        self.stage("hello.txt", "synthetic content\n")
        key = "gh" + "p_" + "A" * 36
        self.run_git("-c", "core.hooksPath=/dev/null", "commit", "-qm", key)
        oid = self.run_git("rev-parse", "HEAD").stdout.strip()
        result = self.push_hook(oid)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(key, result.stderr)
        self.run_git("commit", "--amend", "-qm", "clean", ack=True)
        self.run_git("tag", "-a", "synthetic", "-m", key)
        tag = self.run_git("rev-parse", "synthetic").stdout.strip()
        result = self.push_hook(tag)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("outgoing annotated tag", result.stderr)
        self.assertNotIn(key, result.stderr)

    def test_commit_message_secret_is_blocked_before_creating_history(self):
        self.stage("hello.txt", "synthetic public content\n")
        key = "gh" + "p_" + "A" * 36
        result = self.run_git("commit", "-qm", key, ack=True, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("commit message", result.stderr)
        self.assertNotIn(key, result.stderr)
        self.assertNotEqual(self.run_git("rev-parse", "--verify", "HEAD", check=False).returncode, 0)

    def test_push_scans_nested_annotated_tag_messages(self):
        self.stage("hello.txt", "synthetic content\n")
        self.run_git("commit", "-qm", "clean", ack=True)
        key = "gh" + "p_" + "A" * 36
        self.run_git("tag", "-a", "inner", "-m", key)
        self.run_git("tag", "-a", "outer", "inner", "-m", "clean outer tag")
        oid = self.run_git("rev-parse", "outer").stdout.strip()
        result = self.push_hook(oid)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("outgoing annotated tag", result.stderr)
        self.assertNotIn(key, result.stderr)

    def test_redirected_remote_cannot_hide_secret_history(self):
        key = "gh" + "p_" + "A" * 36
        self.stage("config.txt", key + "\n")
        self.run_git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "unsafe fixture")
        oid = self.run_git("rev-parse", "HEAD").stdout.strip()
        self.run_git("remote", "add", "origin", "https://example.invalid/old.git")
        self.run_git("update-ref", "refs/remotes/origin/main", oid)
        self.assertNotEqual(self.push_hook(oid, remote="origin").returncode, 0)

    def test_stale_tracking_ref_cannot_hide_new_ref_history(self):
        key = "gh" + "p_" + "A" * 36
        self.stage("config.txt", key + "\n")
        self.run_git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "unsafe fixture")
        oid = self.run_git("rev-parse", "HEAD").stdout.strip()
        destination = "https://example.invalid/public.git"
        self.run_git("remote", "add", "origin", destination)
        self.run_git("update-ref", "refs/remotes/origin/stale", oid)
        result = self.push_hook(oid, remote="origin", destination=destination)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("possible GitHub token", result.stderr)

    def test_replaced_blob_cannot_hide_staged_or_outgoing_secret(self):
        key = "gh" + "p_" + "A" * 36
        self.stage("unsafe.txt", key + "\n")
        unsafe = self.run_git("rev-parse", ":unsafe.txt").stdout.strip()
        self.stage("clean.txt", "clean fixture\n")
        clean = self.run_git("rev-parse", ":clean.txt").stdout.strip()
        self.run_git("replace", unsafe, clean)
        result = self.run_git("commit", "-qm", "fixture", ack=True, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("possible GitHub token", result.stderr)
        self.assertNotIn(key, result.stderr)
        self.run_git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "unsafe fixture")
        oid = self.run_git("rev-parse", "HEAD").stdout.strip()
        result = self.push_hook(oid)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("possible GitHub token", result.stderr)
        self.assertNotIn(key, result.stderr)

    def test_history_boundaries_cannot_hide_deleted_ancestor_secret(self):
        key = "gh" + "p_" + "A" * 36
        self.stage("unsafe.txt", key + "\n")
        self.run_git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "unsafe fixture")
        self.run_git("rm", "unsafe.txt")
        self.run_git("commit", "-qm", "clean tip", ack=True)
        oid = self.run_git("rev-parse", "HEAD").stdout.strip()
        for name, diagnostic in [("info/grafts", "Git grafts"), ("shallow", "Shallow history")]:
            with self.subTest(boundary=name):
                path = self.root / ".git" / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(oid + "\n")
                result = self.push_hook(oid)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(diagnostic, result.stderr)
                self.assertNotIn(key, result.stderr)
                path.unlink()

    def test_push_scans_author_and_committer_headers(self):
        key = "gh" + "p_" + "A" * 36
        self.stage("safe.txt", "public fixture\n")
        self.run_git("-c", "core.hooksPath=/dev/null", "-c", "user.name=" + key,
                     "commit", "-qm", "clean message")
        oid = self.run_git("rev-parse", "HEAD").stdout.strip()
        result = self.push_hook(oid)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("outgoing commit object", result.stderr)
        self.assertNotIn(key, result.stderr)

    def test_push_rejects_malformed_input(self):
        result = self.push_hook("not-an-object-id")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Invalid push object ID", result.stderr)


if __name__ == "__main__":
    unittest.main()
