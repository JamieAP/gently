#!/usr/bin/env python3
"""Release gates for tagged builds, synthetic only.

Subcommands, each fail-closed:
  version   tag, workspace version, CHANGELOG heading and build host agree
  package   deterministic archive with no local source paths in the binary
  install   checksum-verified, member-checked extraction of one archive
  previous  the source ref a release must upgrade from
  upgrade   state written by the previous binary survives replacement by the
            new one at the same path, then backs up and restores exactly

No real HOME, credential, collector or session content is used. Output contains
phase names and digests only, never child output.
"""

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import tomllib

REPO = Path(__file__).resolve().parents[1]
TAG = re.compile(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)")
HEADING = re.compile(r"## \[([^\]]+)\](?: - (\d{4}-\d{2}-\d{2}))?")
DOCUMENTS = ("LICENSE", "README.md", "CHANGELOG.md")
# The first release has no earlier tag; it upgrades from the last pre-1.0
# source install on main, after which the v1 readiness work landed.
FIRST_RELEASE_BASELINE = "dedfbb60162849540b485775d9a0b839ea40a097"


class CheckFailure(Exception):
    pass


def require(condition, message):
    if not condition:
        raise CheckFailure(message)


def phase(name):
    print("PASS " + name, flush=True)


def run(args, env=None, ok=True, stdin=None, cwd=None):
    result = subprocess.run([str(arg) for arg in args], cwd=cwd or REPO, env=env, input=stdin,
                            capture_output=True, timeout=60)
    require(not ok or result.returncode == 0, f"{Path(str(args[0])).name} {args[1] if len(args) > 1 else ''} failed (output withheld)")
    return result


def version_of(tag):
    require(TAG.fullmatch(tag or ""), "tag must look like vMAJOR.MINOR.PATCH")
    return tag[1:]


def workspace_version():
    manifest = tomllib.loads((REPO / "Cargo.toml").read_text(encoding="utf-8"))
    return manifest.get("workspace", {}).get("package", {}).get("version")


def changelog_release():
    """Return (version, date) of the newest released CHANGELOG heading."""
    for line in (REPO / "CHANGELOG.md").read_text(encoding="utf-8").splitlines():
        match = HEADING.fullmatch(line.strip())
        if match and match.group(1) != "Unreleased":
            return match.group(1), match.group(2)
    return None, None


def archive_name(version, target):
    return f"gently-{version}-{target}"


def binary_version(binary):
    return run([binary, "--version"]).stdout.decode().strip()


def check_version(args):
    version = version_of(args.tag)
    require(workspace_version() == version, "workspace version does not match the tag")
    released, dated = changelog_release()
    require(released == version and dated is not None, "CHANGELOG's newest release heading does not match the tag")
    if args.target:
        host = [line.split(": ", 1)[1] for line in run(["rustc", "-vV"]).stdout.decode().splitlines()
                if line.startswith("host: ")]
        require(host == [args.target], "build host does not match the release target label")
    phase(f"tag, workspace version and CHANGELOG agree on {version}")


def check_package(args):
    version = version_of(args.tag)
    binary = Path(args.binary)
    require(binary_version(binary) == f"gently {version}", "binary reports a different version")
    data = binary.read_bytes()
    for prefix in args.forbid:
        require(prefix and prefix.encode() not in data, "binary embeds a local build path")
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH") or
                run(["git", "log", "-1", "--format=%ct", "HEAD"]).stdout.decode().strip())
    name = archive_name(version, args.target)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.PAX_FORMAT) as tar:
        def add(arcname, payload, mode, kind=tarfile.REGTYPE):
            info = tarfile.TarInfo(arcname)
            info.type, info.mode, info.mtime = kind, mode, epoch
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            if kind == tarfile.REGTYPE:
                info.size = len(payload)
                tar.addfile(info, io.BytesIO(payload))
            else:
                tar.addfile(info)
        add(name, b"", 0o755, tarfile.DIRTYPE)
        add(f"{name}/gently", data, 0o755)
        for document in DOCUMENTS:
            add(f"{name}/{document}", (REPO / document).read_bytes(), 0o644)
    path = out / f"{name}.tar.gz"
    with path.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
        compressed.write(buffer.getvalue())
    phase(f"{path.name} sha256 {hashlib.sha256(path.read_bytes()).hexdigest()}")


def check_install(args):
    version = version_of(args.tag)
    name = archive_name(version, args.target)
    dist = Path(args.dist)
    archive = dist / f"{name}.tar.gz"
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    listed = {}
    for line in (dist / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  (\S+)", line)
        require(match is not None, "SHA256SUMS has an unexpected line")
        require(match.group(2) not in listed, "SHA256SUMS lists an archive twice")
        listed[match.group(2)] = match.group(1)
    require(listed.get(archive.name) == digest, "archive checksum does not match SHA256SUMS")
    expected = {name: tarfile.DIRTYPE, f"{name}/gently": tarfile.REGTYPE,
                **{f"{name}/{document}": tarfile.REGTYPE for document in DOCUMENTS}}
    dest = Path(args.dest)
    dest.mkdir(parents=True, exist_ok=True)
    installed = dest / "gently"
    with tarfile.open(archive, mode="r:gz") as tar:
        members = tar.getmembers()
        require({m.name: m.type for m in members} == expected and len(members) == len(expected),
                "archive members differ from the release layout")
        source = tar.extractfile(f"{name}/gently")
        with installed.open("wb") as handle:
            shutil.copyfileobj(source, handle)
    installed.chmod(0o755)
    require(binary_version(installed) == f"gently {version}", "installed binary reports a different version")
    phase(f"{archive.name} checksum {digest} and layout verified; installed gently {version}")


def check_previous(args):
    version = version_of(args.tag)
    current = tuple(map(int, version.split(".")))
    tags = run(["git", "tag", "--list", "v*", "--merged", "HEAD"]).stdout.decode().split()
    earlier = sorted((tuple(map(int, t[1:].split("."))), t) for t in tags
                     if TAG.fullmatch(t) and tuple(map(int, t[1:].split("."))) < current)
    print(earlier[-1][1] if earlier else FIRST_RELEASE_BASELINE)


def check_upgrade(args):
    previous, new = Path(args.previous).resolve(), Path(args.binary).resolve()
    require(previous.is_file() and new.is_file(), "both binaries must exist")
    with tempfile.TemporaryDirectory(prefix="gently-upgrade-") as temporary:
        root = Path(temporary)
        home = root / "home"
        home.mkdir()
        installed = root / "bin" / "gently"
        installed.parent.mkdir()
        base = {"HOME": str(home), "XDG_CONFIG_HOME": str(home / "config"),
                "PATH": os.pathsep.join(["/usr/bin", "/bin", "/usr/sbin", "/sbin"]), "NO_COLOR": "1"}

        def state(label):
            directory = root / label
            directory.mkdir()
            (directory / "config.toml").write_text(
                'collector_url = "http://127.0.0.1:9"\nprefer_quic = false\ntenant_id = "release"\n'
                'device_id = "check"\nexport_timeout_secs = 1\nquery_timeout_secs = 2\n')
            return {**base, "GENTLY_STATE_DIR": str(directory)}, directory / "tenants/release/devices/check/state.db"

        original, original_db = state("original")
        restored, restored_db = state("restored")

        def gently(*arguments, env=original, ok=True, stdin=None):
            return run([installed, *arguments], env=env, ok=ok, stdin=stdin)

        def hook(harness, session, event, env=original):
            payload = {"hook_event_name": event, "session_id": session}
            if event == "UserPromptSubmit":
                payload["prompt"] = "invented release-check prompt"
            result = gently("hook", "--harness", harness, env=env, stdin=json.dumps(payload).encode())
            require(result.stdout == b"", "hook emitted stdout")

        def rows(db):
            with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as connection:
                return [row[0] for row in connection.execute("SELECT id FROM outbox ORDER BY id")]

        def registered():
            texts = [path.read_text(encoding="utf-8") for path in
                     (home / ".claude/settings.json", home / ".codex/config.toml") if path.is_file()]
            require(len(texts) == 2, "init did not register both harnesses")
            return texts

        shutil.copy2(previous, installed)
        for harness in ("claude", "codex"):
            gently("init", "--" + harness)
            for event in ("SessionStart", "UserPromptSubmit", "Stop"):
                hook(harness, f"release-{harness}", event)
        before = rows(original_db)
        require(before, "previous binary queued no metadata")
        require(all(str(installed) in text for text in registered()), "hooks do not use the stable path")
        phase(f"previous binary initialized both harnesses and queued {len(before)} envelopes")

        # Replace through a new inode, as an installer does; rewriting a signed
        # Mac executable in place can get the new process killed.
        staged = installed.with_name("gently.new")
        shutil.copy2(new, staged)
        os.replace(staged, installed)
        for harness in ("claude", "codex"):
            gently("init", "--" + harness)
        gently("config", "--check")
        health = json.loads(gently("status", "--json").stdout)
        require(isinstance(health, dict), "status --json is not an object")
        require(rows(original_db) == before, "upgrade changed queued metadata")
        hook("claude", "release-claude", "UserPromptSubmit")
        after = rows(original_db)
        require(after[:len(before)] == before and len(after) > len(before), "new binary did not append to existing state")
        require(all(str(installed) in text for text in registered()), "re-init lost the stable hook path")
        phase(f"new binary re-initialized in place, opened existing state and queued {len(after) - len(before)} more")

        backup = root / "backup.db"
        gently("state", "backup", backup)
        require(backup.stat().st_mode & 0o777 == 0o600, "backup is not owner-only")
        gently("state", "restore", backup, env=restored)
        gently("state", "restore", backup, env=restored, ok=False)
        gently("config", "--check", env=restored)
        require(rows(restored_db) == after, "restore did not reproduce the queued metadata")
        phase(f"backup restored {len(after)} envelopes exactly and refused to overwrite existing state")

        for harness in ("claude", "codex"):
            gently("uninstall", "--" + harness)
        require(not any(str(installed) in text for text in registered()), "uninstall left managed registrations")
        phase("uninstall removed this executable's registrations")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    version = commands.add_parser("version")
    version.add_argument("--tag", required=True)
    version.add_argument("--target")
    package = commands.add_parser("package")
    package.add_argument("--tag", required=True)
    package.add_argument("--target", required=True)
    package.add_argument("--binary", required=True)
    package.add_argument("--out", required=True)
    package.add_argument("--forbid", action="append", default=[], help="path prefix the binary must not embed")
    install = commands.add_parser("install")
    install.add_argument("--tag", required=True)
    install.add_argument("--target", required=True)
    install.add_argument("--dist", required=True)
    install.add_argument("--dest", required=True)
    previous = commands.add_parser("previous")
    previous.add_argument("--tag", required=True)
    upgrade = commands.add_parser("upgrade")
    upgrade.add_argument("--previous", required=True)
    upgrade.add_argument("--binary", required=True)
    args = parser.parse_args(argv)
    handlers = {"version": check_version, "package": check_package, "install": check_install,
                "previous": check_previous, "upgrade": check_upgrade}
    try:
        handlers[args.command](args)
    except (CheckFailure, OSError, ValueError, tarfile.TarError, sqlite3.Error, subprocess.TimeoutExpired) as error:
        message = str(error) if isinstance(error, CheckFailure) else f"{type(error).__name__} (details withheld)"
        print(f"FAIL {args.command}: {message}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
