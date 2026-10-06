import os
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
        for name in [".env", "capture.jsonl", "state.db", "state.sqlite-wal", "state.sqlite3-shm", "state.sqlite3-journal", ".gently/raw/event.json", ".claude/settings.json", "capture.log", "scripts/__pycache__/module.pyc"]:
            with self.subTest(name=name):
                self.stage(name, "private synthetic content\n")
                result = self.run_git("commit", "-qm", "test", ack=True, check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("private artifact path", result.stderr)
                self.assertNotIn("private synthetic content", result.stderr)
                self.run_git("rm", "--cached", "--", name)

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
        self.run_git("commit", "-qm", key, ack=True)
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
