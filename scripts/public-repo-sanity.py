#!/usr/bin/env python3
"""Public-repository Git gate. Scan Git objects, never working-tree secrets."""

import os
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys

ACK_ENV = "GENTLY_PUBLIC_REPO_SANITY"
MAX_BLOB_BYTES = 5 * 1024 * 1024
SECRET_RULES = (
    ("private key", re.compile(rb"-----BEGIN (?:RSA |EC |OPENSSH |DSA |ENCRYPTED )?PRIVATE KEY-----")),
    ("GitHub token", re.compile(rb"\bgh[pousr]_[A-Za-z0-9]{36,255}\b")),
    ("GitHub fine-grained token", re.compile(rb"\bgithub_pat_[A-Za-z0-9_]{60,255}\b")),
    ("Anthropic API key", re.compile(rb"\bsk-ant-[A-Za-z0-9_-]{24,255}\b")),
    ("OpenAI API key", re.compile(rb"\bsk-(?:proj-|svcacct-)[A-Za-z0-9_-]{24,255}\b|\bsk-[A-Za-z0-9]{48}\b")),
    ("AWS access key", re.compile(rb"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b")),
    ("Google API key", re.compile(rb"\bAIza[A-Za-z0-9_-]{35}\b")),
    ("age private identity", re.compile(rb"AGE-(?:SECRET-KEY-1|PLUGIN-SE-1)[A-Z0-9]+")),
)
PRIVATE_DIRS = {".gently", ".claude", ".codex", ".aws", ".ssh", "agent-secrets", ".wrangler", "__pycache__", "node_modules", "target"}
PRIVATE_SUFFIXES = (".db", ".db-wal", ".db-shm", ".db-journal", ".sqlite", ".sqlite3", ".jsonl", ".pem", ".p12", ".pfx", ".key", ".log", ".pyc", ".pyo", ".age", ".enc", ".zip", ".tar", ".tgz", ".gz", ".bz2", ".xz", ".zst", ".7z", ".rar", ".har", ".parquet", ".bak", ".backup")


class CheckFailure(Exception):
    pass


def git(*args, failure="Git could not inspect the requested objects; check aborted"):
    result = subprocess.run(["git", "--no-replace-objects", *args], capture_output=True, check=False)
    if result.returncode:
        # Git output might contain object contents or private values. Do not echo it.
        raise CheckFailure(failure)
    return result.stdout


def private_path(path):
    parts = PurePosixPath(path).parts
    name = parts[-1].lower() if parts else ""
    return (
        any(p.lower() in PRIVATE_DIRS for p in parts)
        or bool(parts and parts[0].lower() in {"raw", "state", "traces", "captures", "exports", "backups", "keys", "policy", "tenants"})
        or name == ".env" or name.startswith(".env.") or name == ".envrc"
        or name == ".dev.vars" or name.startswith(".dev.vars.")
        or name.endswith(PRIVATE_SUFFIXES)
        or bool(re.search(r"\.(?:db|sqlite|sqlite3)-(?:wal|shm|journal)$", name))
        or name in {"auth.json", "credentials.json", ".credentials.json"}
    )


def scan_literal(data, label):
    for rule, pattern in SECRET_RULES:
        if pattern.search(data):
            raise CheckFailure(f"{label!r}: possible {rule}; matched value withheld")
    public_home = str(Path.home()).encode()
    if len(public_home) > 3 and (public_home in data or public_home.replace(b"/", b"\\") in data):
        raise CheckFailure(f"{label!r}: local home path; matched value withheld")


def scan_structured(value, label):
    pending = [value]
    while pending:
        item = pending.pop()
        if isinstance(item, dict):
            keys = set(item)
            kind = item.get("type")
            if (
                "resourceSpans" in keys or "resource_spans" in keys
                or {"hook_event_name", "session_id"} <= keys
                or {"context", "ciphertext_b64"} <= keys
                or bool(keys & {"trace_id", "traceId"}) and bool(keys & {"span_id", "spanId", "span_count", "attributes", "resource"})
                or isinstance(kind, str) and kind in {"session_meta", "event_msg", "response_item", "user", "assistant", "progress"} and bool(keys & {"payload", "message"})
            ):
                raise CheckFailure(f"{label!r}: captured agent or telemetry data; values withheld")
            if keys & {"claudeAiOauth", "access_token", "refresh_token", "api_key", "apiKey", "password", "secret", "private_key"} or {"token", "tenant_id", "device_id"} <= keys and item.get("token") != "HOST_CREDENTIAL_FROM_PROVIDER":
                raise CheckFailure(f"{label!r}: credential-shaped JSON; values withheld")
            pending.extend(item.values())
        elif isinstance(item, list):
            pending.extend(item)
        elif isinstance(item, str):
            scan_literal(item.encode("utf-8"), label)


def scan_json(data, label):
    try:
        text = data.decode("utf-8-sig")
    except UnicodeError:
        return
    try:
        try:
            scan_structured(json.loads(text), label)
            return
        except json.JSONDecodeError:
            pass
        # Renamed JSONL still needs inspection. Source files with JSON literals
        # are not parsed as captures unless a whole line is valid JSON.
        records = 0
        source = PurePosixPath(label).suffix.lower() in {".rs", ".py", ".js", ".ts", ".jsx", ".tsx", ".mjs", ".cjs", ".sh", ".c", ".h", ".cpp", ".go"}
        parsed = []
        for line in text.splitlines():
            if not line.strip():
                continue
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                if source:
                    return  # Source with embedded public JSON fixtures.
                continue
            records += 1
            if records > 4096:
                raise CheckFailure(f"{label!r}: structured data exceeds the review limit")
            if source:
                parsed.append(value)
            else:
                scan_structured(value, label)
        for value in parsed:
            scan_structured(value, label)
    except (ValueError, RecursionError):
        raise CheckFailure(f"{label!r}: structured data could not be safely inspected") from None


def scan_bytes(data, label):
    scan_literal(data, label)
    signatures = (b"SQLite format 3\0", b"age-encryption.org/v1\n", b"-----BEGIN AGE ENCRYPTED FILE-----", b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08", b"\x1f\x8b", b"BZh", b"\xfd7zXZ\x00", b"7z\xbc\xaf\x27\x1c", b"Rar!\x1a\x07", b"\x28\xb5\x2f\xfd")
    if data.startswith(signatures) or data[257:262] == b"ustar":
        raise CheckFailure(f"{label!r}: database, encrypted data or archive; contents withheld")
    scan_json(data, label)


def scan_entries(entries, checked):
    for mode, oid, path in entries:
        if private_path(path):
            raise CheckFailure(f"{path!r}: private artifact path")
        if mode == "160000":
            raise CheckFailure(f"{path!r}: submodule contents require a separate public review")
        if oid in checked:
            continue
        size = int(git("cat-file", "-s", oid))
        if size > MAX_BLOB_BYTES:
            raise CheckFailure(f"{path!r}: object exceeds the 5 MiB review limit")
        scan_bytes(git("cat-file", "blob", oid), path)
        checked.add(oid)


def index_entries():
    for record in git("ls-files", "--stage", "-z").split(b"\0"):
        if not record:
            continue
        header, path = record.split(b"\t", 1)
        mode, oid, stage = header.decode("ascii").split()
        if stage != "0":
            raise CheckFailure("Unmerged index entries; resolve conflicts before public review")
        yield mode, oid, os.fsdecode(path)


def tree_entries(commit):
    for record in git("ls-tree", "-r", "-z", commit).split(b"\0"):
        if not record:
            continue
        header, path = record.split(b"\t", 1)
        mode, kind, oid = header.decode("ascii").split()
        if kind not in {"blob", "commit"}:
            raise CheckFailure("Unexpected tree object; public review aborted")
        yield mode, oid, os.fsdecode(path)


def push_commits(lines):
    # rev-list must see the published original parent graph. Local traversal
    # boundaries cannot establish that all outgoing ancestors were inspected.
    if git("rev-parse", "--is-shallow-repository").strip() != b"false":
        raise CheckFailure("Shallow history cannot be verified for public publication")
    graft_path = os.fsdecode(git("rev-parse", "--git-path", "info/grafts").strip())
    if os.path.lexists(graft_path):
        raise CheckFailure("Git grafts prevent verification of publication history")
    for line in lines:
        fields = line.split()
        if len(fields) != 4:
            raise CheckFailure("Malformed pre-push input; public review aborted")
        _, local_oid, _, remote_oid = fields
        if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", local_oid) or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", remote_oid):
            raise CheckFailure("Invalid push object ID; public review aborted")
        if set(local_oid) == {"0"}:
            continue  # Ref deletion sends no new file contents.
        tag_oid = local_oid
        while git("cat-file", "-t", tag_oid).decode().strip() == "tag":
            tag = git("cat-file", "tag", tag_oid)
            scan_bytes(tag, "outgoing annotated tag")
            header = tag.split(b"\n", 1)[0]
            if not re.fullmatch(rb"object ([0-9a-f]{40}|[0-9a-f]{64})", header):
                raise CheckFailure("Malformed annotated tag; public review aborted")
            tag_oid = header.split()[1].decode("ascii")
        commit = git("rev-parse", "--verify", local_oid + "^{commit}").decode().strip()
        args = ["rev-list", "--reverse", commit]
        if set(remote_oid) != {"0"}:
            git("cat-file", "-e", remote_oid + "^{commit}", failure="Advertised remote commit is unavailable locally; fetch that remote and retry; history scan remains required")
            args.append("^" + remote_oid)
        # A zero advertised destination ID means a new ref. Local tracking refs
        # may be stale or from a redirected remote, so scan all reachable history.
        yield from git(*args).decode().splitlines()


def main(argv):
    if os.environ.get(ACK_ENV) != "1":
        raise CheckFailure(
            f"Set {ACK_ENV}=1 on this Git command only after reviewing the change "
            "for publication to this public repository"
        )
    if not argv or argv[0] not in {"pre-commit", "commit-msg", "pre-push"}:
        raise CheckFailure("Expected pre-commit, commit-msg or pre-push mode")
    checked = set()
    if argv[0] == "pre-commit":
        scan_entries(index_entries(), checked)
        git("diff", "--cached", "--check", failure="Staged whitespace or conflict-marker check failed; inspect git diff --cached --check locally")
    elif argv[0] == "commit-msg":
        if len(argv) != 2:
            raise CheckFailure("Expected the commit message file")
        try:
            with open(argv[1], "rb") as message:
                data = message.read(MAX_BLOB_BYTES + 1)
        except OSError:
            raise CheckFailure("Commit message could not be inspected; check aborted") from None
        if len(data) > MAX_BLOB_BYTES:
            raise CheckFailure("Commit message exceeds the public review limit")
        scan_bytes(data, "commit message")
    else:
        if len(argv) != 3:
            raise CheckFailure("Expected the pre-push remote name and URL")
        for commit in dict.fromkeys(push_commits(sys.stdin.read().splitlines())):
            scan_bytes(git("cat-file", "commit", commit), "outgoing commit object")
            scan_entries(tree_entries(commit), checked)
    print(f"gently: public repository sanity check passed ({len(checked)} Git blobs checked)", file=sys.stderr)


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except (CheckFailure, ValueError, UnicodeError) as error:
        print(f"gently: public repository sanity check blocked: {error}", file=sys.stderr)
        sys.exit(1)
