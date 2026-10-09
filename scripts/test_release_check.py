"""Release gate tests; every binary, tag and repository here is synthetic."""
import contextlib
import hashlib
import importlib.util
import io
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).with_name("release-check.py")
SPEC = importlib.util.spec_from_file_location("release_check", SCRIPT)
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


def fake_binary(path, version):
    path.write_text(f"#!/bin/sh\necho 'gently {version}'\n", encoding="utf-8")
    path.chmod(0o755)
    return path


def git(root, *args):
    subprocess.run(["git", "-c", "user.name=Synthetic", "-c", "user.email=synthetic@example.invalid",
                    "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", *args],
                   cwd=root, check=True, capture_output=True)


class ReleaseCheckTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        for name in release.DOCUMENTS:
            (self.root / name).write_text(f"synthetic {name}\n", encoding="utf-8")
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "9.9.9"\n', encoding="utf-8")
        (self.root / "CHANGELOG.md").write_text(
            "# Changelog\n\n## [Unreleased]\n\n## [9.9.9] - 2026-10-09\n\n## [9.9.8] - 2026-10-01\n",
            encoding="utf-8")
        patcher = mock.patch.object(release, "REPO", self.root)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.addCleanup(self.tmp.cleanup)

    def main(self, *argv, env=None):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err), \
                mock.patch.dict("os.environ", env or {"SOURCE_DATE_EPOCH": "1791504000"}):
            code = release.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def package(self, version="9.9.9", target="x86_64-unknown-linux-gnu", out="dist", forbid=()):
        binary = fake_binary(self.root / "gently", version)
        extra = [arg for prefix in forbid for arg in ("--forbid", prefix)]
        return self.main("package", "--tag", "v9.9.9", "--target", target, "--binary", str(binary),
                         "--out", str(self.root / out), *extra)

    def checksums(self, dist):
        lines = [f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}" for p in sorted(dist.glob("*.tar.gz"))]
        (dist / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8")

    def install(self, dist="dist"):
        return self.main("install", "--tag", "v9.9.9", "--target", "x86_64-unknown-linux-gnu",
                         "--dist", str(self.root / dist), "--dest", str(self.root / "installed"))

    def test_version_requires_tag_workspace_and_changelog_to_agree(self):
        self.assertEqual(self.main("version", "--tag", "v9.9.9")[0], 0)
        for tag in ("v9.9.8", "9.9.9", "v9.9.9-rc1", "v09.9.9"):
            with self.subTest(tag=tag):
                self.assertEqual(self.main("version", "--tag", tag)[0], 1)
        (self.root / "CHANGELOG.md").write_text("## [Unreleased]\n\n## [9.9.9]\n", encoding="utf-8")
        self.assertIn("CHANGELOG", self.main("version", "--tag", "v9.9.9")[2])

    def test_version_checks_the_build_host_label(self):
        host = subprocess.CompletedProcess([], 0, stdout=b"rustc 1.99.0\nhost: aarch64-apple-darwin\n")
        with mock.patch.object(release, "run", return_value=host):
            self.assertEqual(self.main("version", "--tag", "v9.9.9", "--target", "aarch64-apple-darwin")[0], 0)
            code, _, err = self.main("version", "--tag", "v9.9.9", "--target", "x86_64-unknown-linux-gnu")
        self.assertEqual(code, 1)
        self.assertIn("build host", err)

    def test_package_is_deterministic_and_installs_after_checksum(self):
        self.assertEqual(self.package()[0], 0)
        first = (self.root / "dist/gently-9.9.9-x86_64-unknown-linux-gnu.tar.gz").read_bytes()
        self.assertEqual(self.package(out="again")[0], 0)
        self.assertEqual(first, (self.root / "again/gently-9.9.9-x86_64-unknown-linux-gnu.tar.gz").read_bytes())
        with tarfile.open(fileobj=io.BytesIO(first), mode="r:gz") as tar:
            members = {m.name: (m.mode, m.uid, m.uname, m.mtime) for m in tar.getmembers()}
        self.assertEqual(members["gently-9.9.9-x86_64-unknown-linux-gnu/gently"], (0o755, 0, "", 1791504000))
        self.checksums(self.root / "dist")
        code, out, _ = self.install()
        self.assertEqual(code, 0)
        self.assertIn("installed gently 9.9.9", out)
        self.assertEqual(subprocess.run([self.root / "installed/gently", "--version"], capture_output=True).stdout,
                         b"gently 9.9.9\n")

    def test_package_refuses_wrong_versions_and_local_paths(self):
        self.assertEqual(self.package(version="9.9.8")[0], 1)
        code, _, err = self.package(forbid=[str(self.root)])
        self.assertEqual(code, 0, "the fake binary does not embed the path")
        binary = fake_binary(self.root / "gently", "9.9.9")
        binary.write_text(binary.read_text() + f"# {self.root}/src/main.rs\n", encoding="utf-8")
        code, _, err = self.main("package", "--tag", "v9.9.9", "--target", "t", "--binary", str(binary),
                                 "--out", str(self.root / "dist"), "--forbid", str(self.root))
        self.assertEqual(code, 1)
        self.assertIn("local build path", err)

    def test_install_refuses_tampered_or_unlisted_archives(self):
        self.assertEqual(self.package()[0], 0)
        dist = self.root / "dist"
        archive = dist / "gently-9.9.9-x86_64-unknown-linux-gnu.tar.gz"
        self.checksums(dist)
        archive.write_bytes(archive.read_bytes() + b"\0")
        self.assertIn("checksum", self.install()[2])
        self.checksums(dist)
        (dist / "SHA256SUMS").write_text((dist / "SHA256SUMS").read_text() * 2, encoding="utf-8")
        self.assertIn("twice", self.install()[2])
        (dist / "SHA256SUMS").write_text("not a checksum line\n", encoding="utf-8")
        self.assertEqual(self.install()[0], 1)

    def test_install_refuses_unexpected_members(self):
        dist = self.root / "dist"
        dist.mkdir()
        name = "gently-9.9.9-x86_64-unknown-linux-gnu"
        binary = fake_binary(self.root / "gently", "9.9.9")
        with tarfile.open(dist / f"{name}.tar.gz", "w:gz") as tar:
            tar.add(binary, f"{name}/gently")
            for document in release.DOCUMENTS:
                tar.add(self.root / document, f"{name}/{document}")
            tar.add(binary, "../escape")
        self.checksums(dist)
        self.assertIn("release layout", self.install()[2])
        self.assertFalse((self.root / "escape").exists())

    def test_previous_is_the_latest_earlier_tag_or_the_first_release_baseline(self):
        git(self.root, "init", "-q")
        for tag in ("v1.0.0", "v1.1.0", "v1.2.0"):
            git(self.root, "commit", "-q", "--allow-empty", "-m", tag)
            git(self.root, "tag", tag)
        self.assertEqual(self.main("previous", "--tag", "v1.2.0")[1].strip(), "v1.1.0")
        self.assertEqual(self.main("previous", "--tag", "v1.0.0")[1].strip(), release.FIRST_RELEASE_BASELINE)

    def test_first_release_baseline_is_a_commit_in_this_repository(self):
        repo = SCRIPT.resolve().parents[1]
        shallow = subprocess.run(["git", "rev-parse", "--is-shallow-repository"], cwd=repo,
                                 capture_output=True, text=True).stdout.strip()
        if shallow != "false":
            self.skipTest("shallow checkout; the release workflow's full checkout resolves the baseline")
        kind = subprocess.run(["git", "cat-file", "-t", release.FIRST_RELEASE_BASELINE], cwd=repo,
                              capture_output=True, text=True)
        self.assertEqual(kind.stdout.strip(), "commit")


if __name__ == "__main__":
    unittest.main()
