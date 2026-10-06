#!/usr/bin/env python3
"""Real CLI -> age -> Wrangler/workerd/D1 -> CLI/MCP acceptance, synthetic only.

Requires a built gently binary, Node, and `npm ci` in worker/. No real HOME,
credential provider, hardware key, cloud account or remote resource is used.
Output deliberately contains phase names only, never child output or fixtures.
"""

import argparse
import base64
import copy
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import socket
import sqlite3
import subprocess
import tempfile
import termios
import time
import urllib.error
import urllib.request

REPO = Path(__file__).resolve().parents[1]
PASSPHRASE = "invented-acceptance-passphrase-never-a-real-secret"
CANARIES = ["invented-claude-raw-canary", "invented-codex-raw-canary", "invented-epoch-two-canary"]
TOKENS = ["invented-ingest-alpha", "invented-read-alpha", "invented-ingest-beta", "invented-read-beta", "invented-other-writer", "invented-read-backup"]


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def phase(name):
    print("PASS " + name, flush=True)


def command(args, env, cwd=REPO, ok=True, stdin=None):
    result = subprocess.run([str(arg) for arg in args], cwd=cwd, env=env,
                            input=stdin, capture_output=True, timeout=45)
    require(not ok or result.returncode == 0, "child command failed (output withheld)")
    return result


def private_terminal(args, env, prompts, stdin=None, ok=True):
    """Answer immediately on observing a prompt, with no echo-race masking delay.

    For MCP, stdin/stdout remain pipes and /dev/tty is an independent private
    controlling terminal. This exercises the actual stdio protocol, not a mock.
    """
    master, slave = pty.openpty()

    def setup():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    child = subprocess.Popen([str(arg) for arg in args], env=env, cwd=REPO,
                             stdin=slave if stdin is None else subprocess.PIPE,
                             stdout=slave if stdin is None else subprocess.PIPE,
                             stderr=slave, preexec_fn=setup)
    os.close(slave)
    terminal_output = bytearray()
    pipe_output = bytearray()
    answered = cursor = 0
    reads = {master: terminal_output}
    if stdin is not None:
        child.stdin.write(stdin)
        child.stdin.close()
        reads[child.stdout.fileno()] = pipe_output
    deadline = time.monotonic() + 90
    try:
        while reads:
            require(time.monotonic() < deadline, "private terminal timed out")
            for fd in select.select(list(reads), [], [], 0.1)[0]:
                try:
                    block = os.read(fd, 8192)
                except OSError:
                    block = b""
                if not block:
                    del reads[fd]
                else:
                    reads[fd].extend(block)
            while answered < len(prompts):
                found = terminal_output.find(prompts[answered].encode(), cursor)
                if found < 0:
                    break
                cursor = found + len(prompts[answered])
                require(not termios.tcgetattr(master)[3] & termios.ECHO,
                        "passphrase prompt became visible while echo was enabled")
                os.write(master, PASSPHRASE.encode() + b"\n")
                answered += 1
            if child.poll() is not None and not select.select(list(reads), [], [], 0)[0]:
                break
        child.wait(timeout=5)
        require((child.returncode == 0) == ok and answered == len(prompts),
                "private terminal command or prompt contract failed")
        require(PASSPHRASE.encode() not in terminal_output + pipe_output,
                "passphrase appeared in child output")
        require(termios.tcgetattr(master)[3] & termios.ECHO,
                "terminal echo was not restored")
        return bytes(terminal_output if stdin is None else pipe_output).decode()
    finally:
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
        os.close(master)
        if child.stdout is not None:
            child.stdout.close()


def http(base, token, path, body=None):
    request = urllib.request.Request(base + path,
                                    data=None if body is None else json.dumps(body).encode(),
                                    headers={"Authorization": "Bearer " + token,
                                             "User-Agent": "Gently-acceptance/0.1.0 (+https://github.com/JamieAP/gently)",
                                             "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            return response.status, json.loads(response.read())
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read())


def stop_group(child):
    # A child may exit before its descendants; still signal the owned group.
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(child.pid, sig)
        except ProcessLookupError:
            break
        if sig == signal.SIGTERM:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
        else:
            child.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=REPO / "target/debug/gently")
    parser.add_argument("--hardware", action="store_true",
                        help="enroll a new disposable Mac Secure Enclave primary reader; requires operator authorization")
    options = parser.parse_args()
    binary = options.binary.resolve()
    node = shutil.which("node")
    wrangler = REPO / "worker/node_modules/wrangler/bin/wrangler.js"
    require(binary.is_file() and node is not None and wrangler.is_file(),
            "build gently and install Worker dependencies before acceptance")
    with tempfile.TemporaryDirectory(prefix="gently-acceptance-") as temporary:
        root = Path(temporary)
        home = root / "home"
        home.mkdir()
        env = {"HOME": str(home), "XDG_CONFIG_HOME": str(home / "config"),
               "PATH": os.pathsep.join([str(Path(node).parent), "/usr/bin", "/bin", "/usr/sbin", "/sbin"]),
               "WRANGLER_SEND_METRICS": "false", "WRANGLER_WRITE_LOGS": "false",
               "WRANGLER_LOG": "error", "NO_COLOR": "1"}
        principals = [
            {"token": TOKENS[0], "tenant_id": "lab", "device_id": "capture", "capabilities": ["ingest"]},
            {"token": TOKENS[1], "tenant_id": "lab", "device_id": "reader", "capabilities": ["read"]},
            {"token": TOKENS[2], "tenant_id": "work", "device_id": "capture", "capabilities": ["ingest"]},
            {"token": TOKENS[3], "tenant_id": "work", "device_id": "reader", "capabilities": ["read"]},
            {"token": TOKENS[4], "tenant_id": "lab", "device_id": "other", "capabilities": ["ingest"]},
            {"token": TOKENS[5], "tenant_id": "lab", "device_id": "backup", "capabilities": ["read"]},
        ]
        worker_env = {**env, "GENTLY_HOSTS": json.dumps(principals)}
        wrangler_args = [node, wrangler]
        d1_args = [*wrangler_args, "d1", "execute", "gently", "--local", "--config",
                   REPO / "worker/wrangler.local.toml", "--persist-to", root / "d1"]
        command([*d1_args, "--file", REPO / "worker/schema.sql"], worker_env, REPO / "worker")
        phase("fresh actual Wrangler/D1 schema and synthetic per-host credentials")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = "http://127.0.0.1:" + str(port)
        log_path = root / "worker.log"
        log = log_path.open("wb")

        def start_worker():
            child = subprocess.Popen([*map(str, wrangler_args), "dev", "--config",
                                      str(REPO / "worker/wrangler.local.toml"), "--local", "--ip", "127.0.0.1",
                                      "--port", str(port), "--inspector-port", "0", "--persist-to",
                                      str(root / "d1"), "--env-file", "/dev/null"],
                                     env=worker_env, cwd=REPO / "worker", stdout=log, stderr=log,
                                     start_new_session=True)
            deadline = time.monotonic() + 45
            try:
                while True:
                    require(child.poll() is None, "Wrangler exited before readiness")
                    try:
                        if http(base, TOKENS[1], "/v1/query?tenant_id=lab&op=traces")[0] == 200:
                            return child
                    except (OSError, urllib.error.URLError):
                        pass
                    require(time.monotonic() <= deadline, "Wrangler readiness timed out")
                    time.sleep(0.1)
            except BaseException:
                stop_group(child)
                raise

        worker = start_worker()
        try:
            states = {}
            for label, tenant, device in [("capture", "lab", "capture"), ("reader", "lab", "reader"),
                                          ("backup", "lab", "backup"), ("beta", "work", "capture")]:
                state = root / label
                state.mkdir()
                (state / "config.toml").write_text(
                    f'collector_url = "{base}"\nprefer_quic = false\ntenant_id = "{tenant}"\n'
                    f'device_id = "{device}"\nexport_timeout_secs = 1\nquery_timeout_secs = 2\n')
                states[label] = {**env, "GENTLY_STATE_DIR": str(state)}
            capture, reader, backup = states["capture"], states["reader"], states["backup"]
            for args in [["init", "--claude"], ["init", "--codex", "--resolve-raw-values"],
                         ["init", "--codex", "--resolve-raw-values"]]:
                command([binary, *args], capture)
            resolved = json.loads(command([binary, "config", "--json"], capture).stdout)
            require(resolved["tenant_id"] == "lab" and resolved["device_id"] == "capture",
                    "canonical public config ignored file namespaces")
            override = json.loads(command([binary, "config", "--json"],
                                          {**capture, "GENTLY_DEVICE_ID": "override"}).stdout)
            require(override["device_id"] == "override" and "token" not in override,
                    "public config precedence or credential omission failed")
            phase("real CLI init/re-init for both harnesses and public config precedence")

            identities, recipients = [], []
            for label, device_env in [("reader", reader), ("backup", backup)]:
                identity = root / (label + ".age")
                if options.hardware and label == "reader":
                    plugin = shutil.which("age-plugin-se")
                    require(plugin is not None, "install the Mac reader plugin before hardware acceptance")
                    command([plugin, "keygen", "--access-control", "any-biometry-or-passcode",
                             "--recipient-type", "tag", "--output", identity], device_env)
                    identity.chmod(0o600)
                    recipient = command([plugin, "recipients", "--input", identity,
                                         "--recipient-type", "tag"], device_env).stdout.decode().strip()
                    require(recipient.startswith("age1tag1"), "hardware reader did not produce a native tag recipient")
                    recipients.append(recipient)
                else:
                    output = private_terminal([binary, "raw", "identity", "--out", identity], device_env,
                                              ["Reader passphrase: ", "Confirm reader passphrase: "])
                    recipients.append(output.split("Recipient: ", 1)[1].splitlines()[0].strip())
                identities.append(identity)
                require(identity.stat().st_mode & 0o777 == 0o600 and
                        (options.hardware and label == "reader" or
                         identity.read_bytes().startswith(b"age-encryption.org/v1\n")),
                        "identity encryption or permissions failed")
            def reader_prompts(index=0):
                return [] if options.hardware and index == 0 else ["Reader passphrase: "]
            owner = root / "owner.age"
            owner_public = command([binary, "raw", "owner-key", "--recipient", recipients[0],
                                    "--recipient", recipients[1], "--out", owner], reader).stdout.decode()
            owner_public = owner_public.split("Owner public key: ", 1)[1].strip()
            unsigned, manifest, trust = [root / name for name in ["unsigned.json", "manifest.json", "trust.json"]]

            def policy(epoch, enrolled, signer):
                unsigned.write_text(json.dumps({"version": 1, "tenant_id": "lab", "key_epoch": epoch,
                    "expires_unix_secs": int(time.time()) + 600,
                    "readers": [{"device_id": ["reader", "backup"][index], "key_id": "key" + str(index),
                                 "recipient": recipients[index]} for index in enrolled]}))
                private_terminal([binary, "raw", "sign", "--manifest", unsigned, "--owner-key", owner,
                                  "--identity", identities[signer], "--out", manifest], reader,
                                 reader_prompts(signer))
                command([binary, "raw", "trust", "--manifest", manifest, "--owner-public", owner_public,
                         "--out", trust], capture)

            policy(1, [0, 1], 1)  # Recovery device must be able to unlock the owner backup.
            phase("immediate private prompts, encrypted identities and recovery owner signing")
            hook_env = {**capture, "GENTLY_CAPTURE_RAW_VALUES": "1", "GENTLY_RAW_MANIFEST": str(manifest),
                        "GENTLY_RAW_TRUST": str(trust)}

            def hook(harness, payload, environment=hook_env):
                result = command([binary, "hook", "--harness", harness], environment,
                                 stdin=json.dumps(payload).encode())
                require(result.stdout == b"", "hook emitted stdout")

            for harness, canary in zip(["claude", "codex"], CANARIES):
                session = "acceptance-" + harness
                for payload in [{"hook_event_name": "UserPromptSubmit", "session_id": session, "prompt": canary},
                                {"hook_event_name": "Stop", "session_id": session}]:
                    hook(harness, payload)
            db_path = Path(capture["GENTLY_STATE_DIR"]) / "tenants/lab/devices/capture/state.db"

            def scalar(sql):
                with sqlite3.connect(db_path) as db:
                    return db.execute(sql).fetchone()[0]

            require(scalar("SELECT count(*) FROM raw_objects") == 4, "both harness raw captures missing")
            export_env = {**capture, "GENTLY_TOKEN": TOKENS[0], "GENTLY_SYNC_RAW_VALUES": "1",
                          "GENTLY_RAW_IDENTITY": str(root / "deliberately-missing-reader")}
            denied = command([binary, "export"], {**export_env, "GENTLY_TOKEN": "invented-invalid"}, ok=False)
            require(denied.returncode != 0 and scalar("SELECT count(*) FROM outbox") > 0,
                    "authentication failure lost queued metadata")
            stop_group(worker)
            outage = command([binary, "export"], export_env, ok=False)
            require(outage.returncode != 0 and scalar("SELECT count(*) FROM raw_objects WHERE synced=0") == 4,
                    "outage lost queued ciphertext")
            worker = start_worker()
            command([binary, "export"], export_env)
            require(scalar("SELECT count(*) FROM outbox") == 0, "keyless retry did not drain metadata")
            phase("both harness captures, actual outage/restart and corrected auth keyless export")

            def query_env(device_env, identity):
                token = TOKENS[5] if device_env is backup else TOKENS[1]
                return {**device_env, "GENTLY_TOKEN": token, "GENTLY_RESOLVE_RAW_VALUES": "1",
                        "GENTLY_RAW_IDENTITY": str(identity)}

            for index, (device_env, identity) in enumerate([(reader, identities[0]), (backup, identities[1])]):
                text = private_terminal([binary, "spans", "--session-id", "acceptance-claude", "--json"],
                                        query_env(device_env, identity), reader_prompts(index))
                require(CANARIES[0] in text, "enrolled reader failed remote historical decryption")
            mcp_requests = [
                {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                    "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "synthetic-acceptance", "version": "1"}}},
                {"jsonrpc": "2.0", "method": "notifications/initialized"},
                {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
                {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "search_spans",
                 "arguments": {"session_id": "acceptance-codex"}}},
            ]
            protocol = "".join(json.dumps(request) + "\n" for request in mcp_requests).encode()
            mcp_output = private_terminal([binary, "mcp"], query_env(reader, identities[0]),
                                          reader_prompts(), stdin=protocol)
            responses = [json.loads(line) for line in mcp_output.splitlines()]
            require(len(responses) == 3 and all("result" in response for response in responses)
                    and CANARIES[1] in mcp_output, "actual MCP encrypted query failed")
            metadata_env = query_env(reader, root / "missing-reader")
            command([binary, "traces", "--json"], metadata_env)
            command([binary, "stats", "--json"], metadata_env)
            command([binary, "mcp"], metadata_env, stdin=json.dumps(mcp_requests[0]).encode() + b"\n")
            phase("enrolled readers, repeated refs, CLI/MCP decryption and lazy metadata access")

            shared = {"resourceSpans": [{"scopeSpans": [{"spans": [{"traceId": "11" * 16,
                      "spanId": "22" * 8, "name": "shared-alpha", "startTimeUnixNano": "1"}]}]}]}
            require(http(base, TOKENS[0], "/v1/traces?tenant_id=lab", shared)[0] == 200,
                    "alpha shared-ID ingestion failed")
            shared["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["name"] = "shared-beta"
            require(http(base, TOKENS[2], "/v1/traces?tenant_id=work", shared)[0] == 200,
                    "beta shared-ID ingestion failed")
            require(http(base, TOKENS[4], "/v1/traces?tenant_id=lab", shared)[0] == 409,
                    "other host overwrote a span identity")
            require(http(base, TOKENS[1], "/v1/query?tenant_id=work&op=spans")[0] == 403 and
                    http(base, TOKENS[0], "/v1/query?tenant_id=lab&op=spans")[0] == 403,
                    "tenant or capability separation failed")
            status, beta_rows = http(base, TOKENS[3], "/v1/query?tenant_id=work&op=spans")
            require(status == 200 and len(beta_rows) == 1 and beta_rows[0]["name"] == "shared-beta",
                    "same span IDs crossed tenant storage")
            with sqlite3.connect(db_path) as db:
                alpha_ref = db.execute("SELECT raw_ref FROM raw_objects LIMIT 1").fetchone()[0]
            require(http(base, TOKENS[3], "/v1/raw-values/" + alpha_ref + "?tenant_id=work")[0] == 404,
                    "raw ciphertext reference crossed tenant storage")
            phase("actual D1 same-ID tenant isolation, immutable host ownership and capability denial")

            # At least 65 queued envelopes forces real 32-span Free-plan admission
            # responses and client bisection, rather than a transport stub.
            for index in range(35):
                session = "batch-" + str(index)
                hook("codex", {"hook_event_name": "UserPromptSubmit", "session_id": session}, capture)
                hook("codex", {"hook_event_name": "Stop", "session_id": session}, capture)
            require(scalar("SELECT count(*) FROM outbox") >= 65, "batch fixture was too small")
            with sqlite3.connect(db_path) as db:
                expected_batch_ids = {
                    span["spanId"]
                    for (envelope,) in db.execute("SELECT span_json FROM outbox")
                    for resource in json.loads(envelope)["resourceSpans"]
                    for scope in resource["scopeSpans"]
                    for span in scope["spans"]
                }
            command([binary, "export"], export_env)
            require(scalar("SELECT count(*) FROM outbox") == 0, "real batch admission/bisection lost rows")
            require(scalar("SELECT count(*) FROM quarantine") == 0, "batch was quarantined instead of delivered")
            status, delivered_rows = http(base, TOKENS[1], "/v1/query?tenant_id=lab&op=spans&limit=1000")
            batch_rows = [row for row in delivered_rows if (row.get("session_id") or "").startswith("batch-")]
            require(status == 200 and {row["span_id"] for row in batch_rows} == expected_batch_ids and
                    {row["session_id"] for row in batch_rows} == {"batch-" + str(index) for index in range(35)},
                    "actual D1 did not retain every expected batch session and span")
            phase("65-plus queued envelopes through actual Free-plan span admission and bisection")

            policy(2, [0], 0)
            hook("claude", {"hook_event_name": "UserPromptSubmit", "session_id": "epoch-two", "prompt": CANARIES[2]})
            hook("claude", {"hook_event_name": "Stop", "session_id": "epoch-two"})
            with sqlite3.connect(db_path) as db:
                original = json.loads(db.execute("SELECT object_json FROM raw_objects WHERE synced=0").fetchone()[0])
            conflict = copy.deepcopy(original)
            conflict["context"]["event"] = "synthetic-conflict"
            require(http(base, TOKENS[0], "/v1/raw-values?tenant_id=lab", conflict)[0] == 200,
                    "raw conflict setup failed")
            command([binary, "export"], export_env)
            require(scalar("SELECT count(*) FROM outbox") == 0 and
                    scalar("SELECT count(*) FROM raw_objects WHERE rejected_status=409") == 1,
                    "raw rejection blocked metadata or failed encrypted retention")
            command([binary, "status"], capture)
            # Correct only this disposable local D1 fault. Production ciphertext
            # remains immutable; operators must investigate before retrying.
            stop_group(worker)
            command([*d1_args, "--command", "DELETE FROM raw_values WHERE tenant_id='lab' AND raw_ref='" +
                     original["context"]["raw_ref"] + "'"], worker_env, REPO / "worker")
            worker = start_worker()
            command([binary, "export", "--retry-raw-quarantine"], export_env)
            require(scalar("SELECT count(*) FROM raw_objects WHERE rejected_status IS NOT NULL") == 0,
                    "explicit raw quarantine retry failed")
            text = private_terminal([binary, "spans", "--session-id", "epoch-two", "--json"],
                                    query_env(reader, identities[0]), reader_prompts())
            require(CANARIES[2] in text, "remaining reader cannot decrypt new epoch")
            revoked_text = private_terminal([binary, "spans", "--session-id", "epoch-two", "--json"],
                                            query_env(backup, identities[1]), ["Reader passphrase: "], ok=False)
            require(CANARIES[2] not in revoked_text, "removed recipient decrypted the new epoch")
            historical = private_terminal([binary, "spans", "--session-id", "acceptance-claude", "--json"],
                                          query_env(backup, identities[1]), ["Reader passphrase: "])
            require(CANARIES[0] in historical, "revocation unexpectedly erased historical reader access")
            # An encrypted identity without a tty must fail only at raw access.
            revoked = command([binary, "spans", "--session-id", "epoch-two", "--json"],
                              query_env(backup, identities[1]), ok=False)
            require(revoked.returncode != 0 and CANARIES[2].encode() not in revoked.stdout,
                    "noninteractive raw access disclosed plaintext")
            worker_env["GENTLY_HOSTS"] = json.dumps(principals[:-1])
            stop_group(worker)
            worker = start_worker()
            require(http(base, TOKENS[5], "/v1/query?tenant_id=lab&op=spans")[0] == 401,
                    "removed host credential retained cloud access")
            phase("retained raw 409 quarantine, metadata progress, explicit retry, revocation and historical access")

            for label in ["capture", "reader", "backup"]:
                for path in Path(states[label]["GENTLY_STATE_DIR"]).rglob("*"):
                    if path.is_file():
                        require(not any(canary.encode() in path.read_bytes() for canary in CANARIES),
                                "plaintext canary appeared in local persistent state")
            for path in (root / "d1").rglob("*"):
                if path.is_file():
                    require(not any(canary.encode() in path.read_bytes() for canary in CANARIES),
                            "plaintext canary appeared in actual D1 persistent state")
            log.flush()
            log_bytes = log_path.read_bytes()
            require(not any(value.encode() in log_bytes for value in [*CANARIES, *TOKENS, PASSPHRASE]),
                    "fixture secret appeared in Worker output")
            status, object_value = http(base, TOKENS[1], "/v1/raw-values/" + original["context"]["raw_ref"] + "?tenant_id=lab")
            require(status == 200 and base64.b64decode(object_value["ciphertext_b64"]).startswith(b"age-encryption.org/v1\n"),
                    "cloud raw envelope was not age ciphertext")
            phase("SQLite/WAL/D1/log canary scans and opaque cloud ciphertext")
        finally:
            stop_group(worker)
            log.close()
        with socket.socket() as sock:
            require(sock.connect_ex(("127.0.0.1", port)) != 0, "Worker listener survived cleanup")
    phase("all disposable state, software keys and owned Worker processes cleaned up")


if __name__ == "__main__":
    main()
