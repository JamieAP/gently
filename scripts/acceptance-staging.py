#!/usr/bin/env python3
"""Review a disposable staging plan; --execute runs it through existing OAuth.

All cloud resources have a fresh nonce. Host credentials stay in memory and
Wrangler receives them over stdin. Only public resource IDs and phase results
can be written to --manifest-out. Secure Enclave prompts remain interactive.
"""


import argparse
import base64
import copy
import importlib.util
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error

REPO = Path(__file__).resolve().parents[1]
CANARIES = ["invented-staging-claude-canary", "invented-staging-codex-canary",
            "invented-staging-new-epoch-canary"]


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def load_local():
    spec = importlib.util.spec_from_file_location("gently_acceptance_local", REPO / "scripts/acceptance-local.py")
    local = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(local)
    return local


def public_plan(options):
    return {
        "account_id": options.account,
        "run_id": options.run_id,
        "worker_name": "gently-accept-" + options.run_id,
        "database_name": "gently-accept-db-" + options.run_id,
        "secure_enclave": options.secure_enclave,
        "actions": [
            "Use existing Wrangler OAuth; reject account mismatch or pre-existing resource names",
            "Create one disposable D1 database and initialize the current schema remotely",
            "Deploy one disposable Worker on the existing account workers.dev subdomain; no routes",
            "Upload randomly generated per-host GENTLY_HOSTS credentials only through stdin",
            "Capture both harnesses; export without reader keys; decrypt remotely fetched ciphertext in CLI/MCP",
            "Verify two tenants sharing IDs, host ownership, capabilities, recovery and recipient revocation",
            "Verify raw rejection/quarantine/retry and credential rotation/revocation",
            "Fetch bounded opaque D1 values for local canary assertions; optionally probe forced HTTP/3",
            "Delete only this run's Worker and the verified created D1 UUID; remove disposable key files",
        ] + (["Generate one new Secure Enclave tag identity with any-biometry-or-passcode; native prompts require the user"]
             if options.secure_enclave else []),
        "scope": "No existing resources, credential vaults, native authentication stores, domains, routes or OAuth settings are edited",
    }


def write_manifest(path, value):
    if path is None:
        return
    # Keep the last complete recovery record if the process is killed mid-write.
    temporary = Path(str(path) + "." + secrets.token_hex(8) + ".tmp")
    try:
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w") as file:
            json.dump(value, file, indent=2)
            file.write("\n")
            file.flush()
            os.fsync(file.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def request_cleanup(_signal, _frame):
    # subprocess.run terminates/waits for its current child on interruption;
    # main's finally then removes only the resources owned by this run.
    raise KeyboardInterrupt


def native_command(args, env, stdin=None, ok=True):
    """Allow the hardware backend's full native-authentication deadline."""
    try:
        result = subprocess.run(list(map(str, args)), env=env, cwd=REPO,
                                input=stdin, capture_output=True, timeout=90)
    except (OSError, subprocess.TimeoutExpired):
        raise RuntimeError("Native authentication command did not complete; child output withheld") from None
    require(not ok or result.returncode == 0, "Native authentication command failed; child output withheld")
    return result


class Cloud:
    def __init__(self, options, root, node, report):
        self.options, self.root, self.report = options, root, report
        self.config = root / "wrangler.staging.json"
        self.wrangler = [node, str(REPO / "worker/node_modules/wrangler/bin/wrangler.js")]
        self.env = {
            "HOME": os.environ["HOME"],
            "PATH": os.pathsep.join([str(Path(node).parent), "/opt/homebrew/bin", "/usr/local/bin",
                                     "/usr/bin", "/bin", "/usr/sbin", "/sbin"]),
            "CLOUDFLARE_ACCOUNT_ID": options.account, "CI": "true", "NO_COLOR": "1",
            "WRANGLER_SEND_METRICS": "false", "WRANGLER_WRITE_LOGS": "false", "WRANGLER_LOG": "log",
        }
        if os.environ.get("XDG_CONFIG_HOME"):
            self.env["XDG_CONFIG_HOME"] = os.environ["XDG_CONFIG_HOME"]
        self.database_id = None
        self.worker_attempted = self.database_attempted = False
        self.write_config()
        self.checkpoint()

    def checkpoint(self):
        write_manifest(self.options.manifest_out, self.report)

    def write_config(self):
        configuration = {
            "$schema": str(REPO / "worker/node_modules/wrangler/config-schema.json"),
            "name": self.report["worker_name"], "account_id": self.options.account,
            "main": str(REPO / "worker/src/index.ts"), "compatibility_date": "2025-01-01",
            "workers_dev": True, "preview_urls": False, "send_metrics": False,
            "observability": {"enabled": False},
        }
        if self.database_id:
            configuration["d1_databases"] = [{"binding": "DB", "database_name": self.report["database_name"],
                                               "database_id": self.database_id}]
        self.config.write_text(json.dumps(configuration))
        self.config.chmod(0o600)

    def call(self, args, label, stdin=None, ok=True):
        try:
            result = subprocess.run([*self.wrangler, *map(str, args), "--config", str(self.config),
                                     "--env-file", "/dev/null"], env=self.env, cwd=self.root,
                                    input=stdin, capture_output=True, timeout=120)
        except (OSError, subprocess.TimeoutExpired):
            raise RuntimeError(label + " did not complete; child output withheld") from None
        require(not ok or result.returncode == 0, label + " failed; child output withheld")
        return result

    def databases(self):
        output = self.call(["d1", "list", "--json"], "D1 inventory").stdout
        try:
            value = json.loads(output)
        except ValueError:
            raise RuntimeError("D1 inventory did not return JSON") from None
        require(isinstance(value, list), "D1 inventory had an unexpected shape")
        return value

    def worker_exists(self):
        result = self.call(["versions", "list", "--name", self.report["worker_name"], "--json"],
                           "Worker inventory", ok=False)
        if result.returncode == 0:
            return True
        require(re.search(rb"\b10007\b", result.stdout + result.stderr) is not None,
                "Cannot prove the disposable Worker name is absent")
        return False

    def preflight(self):
        output = self.call(["whoami", "--json"], "Existing OAuth check").stdout
        try:
            identity = json.loads(output)
        except ValueError:
            raise RuntimeError("OAuth check did not return JSON") from None
        require(any(account.get("id") == self.options.account for account in identity.get("accounts", [])),
                "The selected account is not available through existing Wrangler authentication")
        require(not any(item.get("name") == self.report["database_name"] for item in self.databases()),
                "Refusing to reuse an existing database name")
        require(not self.worker_exists(), "Refusing to deploy over an existing Worker")

    def discover_created_database(self):
        matches = [item for item in self.databases() if item.get("name") == self.report["database_name"]]
        require(len(matches) == 1 and re.fullmatch(r"[0-9a-f-]{36}", matches[0].get("uuid", "")),
                "Cannot identify exactly one newly created D1 database")
        discovered = matches[0]["uuid"]
        require(self.database_id is None or self.database_id == discovered, "Created D1 identity changed")
        self.database_id = discovered
        self.report["database_id"] = discovered
        self.write_config()
        self.checkpoint()

    def create(self):
        self.database_attempted = True
        self.report["database_creation"] = "attempted"
        self.checkpoint()
        result = self.call(["d1", "create", self.report["database_name"], "--location", "weur"],
                           "Disposable D1 creation")
        # Remember a public UUID even if the following inventory request fails.
        match = re.search(rb'"(?:database_id|uuid)"\s*:\s*"([0-9a-f-]{36})"|database_id\s*=\s*"([0-9a-f-]{36})"', result.stdout)
        if match:
            self.database_id = next(group for group in match.groups() if group).decode()
            self.report["database_id"] = self.database_id
            self.write_config()
            self.checkpoint()
        self.discover_created_database()
        self.call(["d1", "execute", "DB", "--remote", "--file", REPO / "worker/schema.sql", "--yes"],
                  "Remote schema initialization")
        self.worker_attempted = True
        self.report["worker_creation"] = "attempted"
        self.checkpoint()
        deployed = self.call(["deploy"], "Disposable Worker deployment")
        urls = re.findall(rb"https://[a-z0-9.-]+\.workers\.dev", deployed.stdout + deployed.stderr)
        candidates = [value.decode() for value in urls if value.decode().startswith("https://" + self.report["worker_name"] + ".")]
        require(candidates, "Deployment did not return this disposable Worker's URL")
        self.report["worker_url"] = candidates[-1]
        self.checkpoint()
        return candidates[-1]

    def hosts(self, principals):
        self.call(["secret", "bulk", "--name", self.report["worker_name"]], "Disposable host credential update",
                  stdin=json.dumps({"GENTLY_HOSTS": json.dumps(principals)}).encode())

    def sql(self, sql):
        output = self.call(["d1", "execute", "DB", "--remote", "--command", sql, "--json"],
                           "Disposable remote database assertion").stdout
        try:
            results = json.loads(output)
        except ValueError:
            raise RuntimeError("Remote database assertion did not return JSON") from None
        require(isinstance(results, list) and all(item.get("success") for item in results),
                "Remote database assertion failed")
        return [row for item in results for row in item.get("results", [])]

    def cleanup(self):
        errors = []
        if self.worker_attempted:
            try:
                if self.worker_exists():
                    self.call(["delete", self.report["worker_name"]], "Disposable Worker deletion")
                require(not self.worker_exists(), "Disposable Worker survived deletion")
            except RuntimeError:
                errors.append("worker")
        if self.database_attempted:
            try:
                matches = [item for item in self.databases() if item.get("name") == self.report["database_name"]]
                if matches:
                    self.discover_created_database()
                    self.call(["d1", "delete", "DB", "--skip-confirmation"], "Disposable D1 deletion")
                require(not any(item.get("uuid") == self.database_id for item in self.databases()),
                        "Disposable database survived deletion")
            except RuntimeError:
                errors.append("database")
        self.report["cleanup"] = "complete" if not errors else "manual attention required: " + ", ".join(errors)
        self.checkpoint()
        return errors


def wait_http(local, base, token, path, expected):
    deadline = time.monotonic() + 120
    last_status = None
    last_error = None
    while time.monotonic() < deadline:
        try:
            status, value = local.http(base, token, path)
            last_status = status
            if status == expected:
                return value
        except (OSError, urllib.error.URLError, ValueError) as error:
            last_error = type(error).__name__
        time.sleep(2)
    raise RuntimeError("Staging route did not reach its expected authorization state "
                       f"(status={last_status}, error={last_error}); response withheld")


def exercise(options, root, cloud, binary, node, report):
    local = load_local()

    def phase(name):
        report.setdefault("passed", []).append(name)
        local.phase(name)

    home = root / "isolated-home"
    home.mkdir()
    env = {"HOME": str(home), "PATH": os.pathsep.join([str(binary.parent), str(Path(node).parent),
           "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"])}
    states = {}
    for label, tenant, device in [("capture", "lab", "capture"), ("reader", "lab", "reader"),
                                   ("backup", "lab", "backup"), ("hardware", "lab", "hardware")]:
        state = root / label
        state.mkdir()
        states[label] = {**env, "GENTLY_STATE_DIR": str(state), "GENTLY_TENANT_ID": tenant,
                         "GENTLY_DEVICE_ID": device}
    identities, recipients = [], []
    for label in ["reader", "backup"]:
        identity = root / (label + ".age")
        output = local.private_terminal([binary, "raw", "identity", "--out", identity], states[label],
                                        ["Reader passphrase: ", "Confirm reader passphrase: "])
        recipients.append(output.split("Recipient: ", 1)[1].splitlines()[0].strip())
        identities.append(identity)
    if options.secure_enclave:
        require(sys.platform == "darwin", "Secure Enclave acceptance requires macOS")
        plugin = shutil.which("age-plugin-se", path=env["PATH"])
        require(plugin is not None, "Install age-plugin-se before Secure Enclave acceptance")
        identity = root / "fresh-secure-enclave.agekey"
        print("ACTION Complete native Touch ID/passcode dialogs for this new disposable identity; cancellation is respected.", flush=True)
        native_command([plugin, "keygen", "--access-control", "any-biometry-or-passcode",
                        "--recipient-type", "tag", "-o", identity], env)
        identity.chmod(0o600)
        public = native_command([plugin, "recipients", "--recipient-type", "tag", "-i", identity], env).stdout.decode().strip()
        require(len(public.splitlines()) == 1 and re.fullmatch(r"age1[a-z0-9]+", public) is not None,
                "Secure Enclave recipient command did not return one native public recipient")
        recipients.append(public)
        identities.append(identity)
    owner = root / "owner.age"
    recipient_args = [argument for recipient in recipients for argument in ["--recipient", recipient]]
    owner_public = local.command([binary, "raw", "owner-key", *recipient_args, "--out", owner], states["reader"]).stdout.decode()
    owner_public = owner_public.split("Owner public key: ", 1)[1].strip()
    unsigned, manifest, trust = [root / name for name in ["unsigned.json", "manifest.json", "trust.json"]]
    labels = ["reader", "backup", "hardware"]
    policy_expiry = int(time.time()) + 3600

    def policy(epoch, enrolled, signer):
        unsigned.write_text(json.dumps({"version": 1, "tenant_id": "lab", "key_epoch": epoch,
            "expires_unix_secs": policy_expiry,
            "readers": [{"device_id": labels[index], "key_id": "key" + str(index),
                         "recipient": recipients[index]} for index in enrolled]}))
        args = [binary, "raw", "sign", "--manifest", unsigned, "--owner-key", owner,
                "--identity", identities[signer], "--out", manifest]
        if signer == 2:
            print("ACTION Approve the native dialog to sign this disposable recipient policy.", flush=True)
            native_command(args, states["hardware"])
        else:
            local.private_terminal(args, states[labels[signer]], ["Reader passphrase: "])
        local.command([binary, "raw", "trust", "--manifest", manifest, "--owner-public", owner_public,
                       "--out", trust], states["capture"])

    policy(1, list(range(len(identities))), 2 if options.secure_enclave else 1)
    # Recovery must independently unlock the encrypted owner; same policy stays pinned.
    policy(1, list(range(len(identities))), 1)
    phase("protected software enrollment, recovery owner signing" + (" and fresh Secure Enclave signing" if options.secure_enclave else ""))
    base = cloud.create()
    tokens = {name: secrets.token_urlsafe(32) for name in ["capture", "reader", "backup", "hardware", "other", "work-capture", "work-reader"]}
    principals = [{"token": token, "tenant_id": "work" if label.startswith("work-") else "lab",
                   "device_id": label.removeprefix("work-"),
                   "capabilities": ["ingest"] if label in ["capture", "other", "work-capture"] else ["read"]}
                  for label, token in tokens.items()]
    cloud.hosts(principals)
    wait_http(local, base, tokens["reader"], "/v1/query?tenant_id=lab&op=traces", 200)
    phase("new remote Worker/D1 with stdin-only random host credentials")
    for label, state_env in states.items():
        Path(state_env["GENTLY_STATE_DIR"], "config.toml").write_text(
            'collector_url = ' + json.dumps(base) + '\nprefer_quic = true\nexport_timeout_secs = 10\nquery_timeout_secs = 10\n')
    capture = states["capture"]
    hook_env = {**capture, "GENTLY_CAPTURE_RAW_VALUES": "1", "GENTLY_RAW_MANIFEST": str(manifest), "GENTLY_RAW_TRUST": str(trust)}

    def hook(harness, payload, environment=hook_env):
        result = local.command([binary, "hook", "--harness", harness], environment, stdin=json.dumps(payload).encode())
        require(result.stdout == b"", "Staging hook emitted stdout")

    for harness, canary in zip(["claude", "codex"], CANARIES):
        for payload in [{"hook_event_name": "UserPromptSubmit", "session_id": "staging-" + harness, "prompt": canary},
                        {"hook_event_name": "Stop", "session_id": "staging-" + harness}]:
            hook(harness, payload)
    db_path = Path(capture["GENTLY_STATE_DIR"]) / "tenants/lab/devices/capture/state.db"

    def scalar(sql):
        with sqlite3.connect(db_path) as db:
            return db.execute(sql).fetchone()[0]

    export_env = {**capture, "GENTLY_TOKEN": tokens["capture"], "GENTLY_SYNC_RAW_VALUES": "1",
                  "GENTLY_RAW_IDENTITY": str(root / "deliberately-missing-reader")}
    local.command([binary, "export"], export_env)
    require(scalar("SELECT count(*) FROM outbox") == 0 and scalar("SELECT count(*) FROM raw_objects WHERE synced=0") == 0,
            "Remote keyless export did not acknowledge the queues")
    phase("both harness capture and keyless export to the real Cloudflare edge")

    def read_env(label):
        return {**states[label], "GENTLY_TOKEN": tokens[label], "GENTLY_RESOLVE_RAW_VALUES": "1",
                "GENTLY_RAW_IDENTITY": str(identities[labels.index(label)])}

    def read(label, session, expected=True, mcp=False):
        args = [binary, "mcp"] if mcp else [binary, "spans", "--session-id", session, "--json"]
        protocol = None
        if mcp:
            requests = [{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
                        {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                         "name": "search_spans", "arguments": {"session_id": session}}}]
            protocol = "".join(json.dumps(value) + "\n" for value in requests).encode()
        if label == "hardware":
            print("ACTION Approve native authentication for encrypted raw " + ("MCP" if mcp else "CLI") + " access.", flush=True)
            result = native_command(args, read_env(label), stdin=protocol, ok=expected)
            require((result.returncode == 0) == expected, "Secure Enclave reader command had an unexpected result")
            return result.stdout.decode()
        return local.private_terminal(args, read_env(label), ["Reader passphrase: "], stdin=protocol, ok=expected)

    for label in labels[:len(identities)]:
        require(CANARIES[0] in read(label, "staging-claude"), "An enrolled remote reader failed decryption")
        require(CANARIES[1] in read(label, "staging-codex", mcp=True), "Encrypted MCP decryption failed")
    metadata_env = {**states["reader"], "GENTLY_TOKEN": tokens["reader"]}
    metadata = local.command([binary, "spans", "--json"], metadata_env).stdout
    require(not any(value.encode() in metadata for value in CANARIES), "Opaque metadata unexpectedly contained raw text")
    phase("remote cache-miss CLI/MCP hydration for each enrolled software/hardware reader")

    shared = {"resourceSpans": [{"scopeSpans": [{"spans": [{"traceId": "11" * 16, "spanId": "22" * 8,
               "name": "shared-lab", "startTimeUnixNano": "1"}]}]}]}
    require(local.http(base, tokens["capture"], "/v1/traces?tenant_id=lab", shared)[0] == 200, "Lab shared-ID ingest failed")
    shared["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["name"] = "shared-work"
    require(local.http(base, tokens["work-capture"], "/v1/traces?tenant_id=work", shared)[0] == 200, "Work shared-ID ingest failed")
    require(local.http(base, tokens["other"], "/v1/traces?tenant_id=lab", shared)[0] == 409, "Another host overwrote span ownership")
    require(local.http(base, tokens["capture"], "/v1/query?tenant_id=lab&op=spans")[0] == 403 and
            local.http(base, tokens["reader"], "/v1/query?tenant_id=work&op=spans")[0] == 403 and
            local.http(base, tokens["reader"], "/v1/traces?tenant_id=lab", shared)[0] == 403,
            "Tenant/capability boundaries failed")
    status, work_rows = local.http(base, tokens["work-reader"], "/v1/query?tenant_id=work&op=spans")
    require(status == 200 and len(work_rows) == 1 and work_rows[0]["name"] == "shared-work", "Shared IDs crossed tenant storage")
    reference = scalar("SELECT raw_ref FROM raw_objects LIMIT 1")
    require(local.http(base, tokens["work-reader"], "/v1/raw-values/" + reference + "?tenant_id=work")[0] == 404,
            "Ciphertext reference crossed tenants")
    status, opaque = local.http(base, tokens["reader"], "/v1/raw-values/" + reference + "?tenant_id=lab")
    require(status == 200 and base64.b64decode(opaque["ciphertext_b64"]).startswith(b"age-encryption.org/v1\n") and
            not any(value in json.dumps(opaque) for value in CANARIES), "Remote object was not opaque age ciphertext")
    phase("real D1 shared-ID tenant isolation, host ownership, capability guards and opaque ciphertext")

    policy(2, [index for index in range(len(identities)) if index != 1], 2 if options.secure_enclave else 0)
    for event in ["UserPromptSubmit", "Stop"]:
        hook("claude", {"hook_event_name": event, "session_id": "staging-epoch-two", "prompt": CANARIES[2]})
    with sqlite3.connect(db_path) as db:
        original = json.loads(db.execute("SELECT object_json FROM raw_objects WHERE synced=0 LIMIT 1").fetchone()[0])
    conflict = copy.deepcopy(original)
    conflict["context"]["event"] = "invented-conflict"
    require(local.http(base, tokens["capture"], "/v1/raw-values?tenant_id=lab", conflict)[0] == 200, "Disposable conflict setup failed")
    local.command([binary, "export"], export_env)
    require(scalar("SELECT count(*) FROM outbox") == 0 and scalar("SELECT count(*) FROM raw_objects WHERE rejected_status=409") == 1,
            "Rejected ciphertext blocked metadata or was not retained")
    raw_ref = original["context"]["raw_ref"]
    cloud.sql("DELETE FROM raw_values WHERE tenant_id='lab' AND raw_ref='" + raw_ref + "'")
    local.command([binary, "export", "--retry-raw-quarantine"], export_env)
    require(scalar("SELECT count(*) FROM raw_objects WHERE rejected_status IS NOT NULL") == 0, "Explicit quarantine retry failed")
    require(CANARIES[2] in read("reader", "staging-epoch-two"), "Remaining reader failed new-epoch decryption")
    if options.secure_enclave:
        require(CANARIES[2] in read("hardware", "staging-epoch-two"), "Secure Enclave reader failed new epoch")
    require(CANARIES[2] not in read("backup", "staging-epoch-two", expected=False), "Removed recipient decrypted a new epoch")
    require(CANARIES[0] in read("backup", "staging-claude"), "Recipient removal incorrectly removed historical access")
    phase("actual raw 409 retention/retry and recipient revocation with explicit historical-access limits")

    old_capture, old_reader = tokens["capture"], tokens["reader"]
    for principal in principals:
        if principal["device_id"] in ["capture", "reader"] and principal["tenant_id"] == "lab":
            tokens[principal["device_id"]] = secrets.token_urlsafe(32)
            principal["token"] = tokens[principal["device_id"]]
    principals = [principal for principal in principals if principal["device_id"] != "backup"]
    cloud.hosts(principals)
    for token in [old_capture, old_reader, tokens["backup"]]:
        wait_http(local, base, token, "/v1/whoami?tenant_id=lab", 401)
    wait_http(local, base, tokens["reader"], "/v1/query?tenant_id=lab&op=traces", 200)
    for event in ["SessionStart", "UserPromptSubmit", "Stop"]:
        hook("codex", {"hook_event_name": event, "session_id": "credential-rotation"}, capture)
    require(scalar("SELECT count(*) FROM outbox") > 0, "Credential rotation fixture did not queue metadata")
    rejected = local.command([binary, "export"], export_env, ok=False)
    require(rejected.returncode != 0 and scalar("SELECT count(*) FROM outbox") > 0, "Revoked credential lost queued metadata")
    export_env["GENTLY_TOKEN"] = tokens["capture"]
    local.command([binary, "export"], export_env)
    require(scalar("SELECT count(*) FROM outbox") == 0 and scalar("SELECT count(*) FROM quarantine") == 0,
            "Rotated credential did not recover queued delivery without quarantine")
    phase("live host credential rotation/revocation and corrected-token durable queue recovery")

    # Never send raw canaries to Cloudflare, even as SQL search parameters.
    # Fetch only this disposable database's bounded opaque values, hold them in
    # memory, and assert locally without printing or saving query output.
    raw_count = cloud.sql("SELECT COUNT(*) AS n FROM raw_values")[0]["n"]
    span_count = cloud.sql("SELECT COUNT(*) AS n FROM spans")[0]["n"]
    require(0 < raw_count <= 32 and 0 < span_count <= 64, "Remote canary scan would exceed its bounded fixture size")
    raw_rows = cloud.sql("SELECT envelope_json FROM raw_values LIMIT 32")
    span_rows = cloud.sql("SELECT attrs_json,resource_json FROM spans LIMIT 64")
    require(len(raw_rows) == raw_count and len(span_rows) == span_count, "Remote canary scan did not inspect every fixture row")
    require(not any(value in json.dumps([raw_rows, span_rows]) for value in CANARIES),
            "Remote D1 contained a plaintext canary")
    for path in root.rglob("*"):
        if path.is_file():
            data = path.read_bytes()
            require(not any(value.encode() in data for value in [*CANARIES, *tokens.values(), old_capture, old_reader, local.PASSPHRASE]),
                    "A private fixture value appeared in a persistent acceptance file")
    phase("opaque D1 values checked locally and local file/WAL/log secret-canary scans")
    probe_http3(local, base, tokens["reader"], env, report, phase)


def probe_http3(local, base, token, env, report, phase):
    for candidate in ["/opt/homebrew/opt/curl/bin/curl", "/usr/local/opt/curl/bin/curl", shutil.which("curl")]:
        if not candidate or not Path(candidate).is_file():
            continue
        version = local.command([candidate, "--version"], env).stdout
        if b"HTTP3" not in version:
            continue
        configuration = 'url = "' + base + '/v1/whoami?tenant_id=lab"\nheader = "Authorization: Bearer ' + token + '"\n'
        result = local.command([candidate, "--config", "-", "--http3-only", "--silent", "--show-error", "--max-time", "15"],
                               env, stdin=configuration.encode(), ok=False)
        if result.returncode == 0:
            require(json.loads(result.stdout).get("httpProtocol") == "HTTP/3", "Forced HTTP/3 probe was not observed at the edge")
            report["http3"] = "verified"
            phase("forced HTTP/3 observed by the deployed Cloudflare edge")
            return
        report["http3"] = "not verified: forced QUIC network path unavailable"
        print("SKIP forced HTTP/3: network path unavailable", flush=True)
        return
    report["http3"] = "not verified: no installed HTTP/3-capable curl"
    print("SKIP forced HTTP/3: no installed HTTP/3-capable curl", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--plan", action="store_true", help="Print public actions only; no network or hardware access (default)")
    mode.add_argument("--execute", action="store_true", help="Create, exercise and clean up the disposable staging resources")
    parser.add_argument("--account", required=True, help="Explicit Cloudflare account ID; no automatic account selection")
    parser.add_argument("--run-id", default=secrets.token_hex(8), help="Reviewed 16-hex nonce shared by plan and execution")
    parser.add_argument("--secure-enclave", action="store_true", help="Enroll a new disposable Mac Secure Enclave identity; approve native dialogs")
    parser.add_argument("--binary", type=Path, default=REPO / "target/debug/gently")
    parser.add_argument("--manifest-out", type=Path, help="Save public resource IDs/results only, never tokens or key material")
    options = parser.parse_args()
    require(re.fullmatch(r"[0-9a-f]{32}", options.account) is not None, "Account must be an explicit 32-hex ID")
    require(re.fullmatch(r"[0-9a-f]{16}", options.run_id) is not None, "Run ID must be a fresh 16-hex nonce")
    report = public_plan(options)
    if not options.execute:
        print(json.dumps(report, indent=2))
        write_manifest(options.manifest_out, report)
        return
    binary, node = options.binary.resolve(), shutil.which("node")
    require(binary.is_file() and node is not None and (REPO / "worker/node_modules/wrangler/bin/wrangler.js").is_file(),
            "Build gently and install Worker dependencies before staging acceptance")
    cloud = None
    failure = None
    previous_sigterm = signal.signal(signal.SIGTERM, request_cleanup)
    try:
        with tempfile.TemporaryDirectory(prefix="gently-staging-") as temporary:
            root = Path(temporary)
            try:
                cloud = Cloud(options, root, node, report)
                cloud.preflight()
                exercise(options, root, cloud, binary, node, report)
            except BaseException as error:
                failure = error
                report["result"] = "failed; child output withheld"
            finally:
                # A second termination request must not abort resource cleanup.
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                errors = cloud.cleanup() if cloud is not None else []
                write_manifest(options.manifest_out, report)
                if errors:
                    print("CLEANUP ATTENTION " + json.dumps({key: report.get(key) for key in ["account_id", "worker_name", "database_name", "database_id", "cleanup"]}), flush=True)
                    failure = RuntimeError("Disposable resource cleanup requires attention; public IDs shown above")
    finally:
        signal.signal(signal.SIGTERM, previous_sigterm)
    if failure is not None:
        if isinstance(failure, RuntimeError):
            raise failure
        raise RuntimeError("Staging acceptance interrupted or failed; child output withheld") from None
    report["result"] = "passed"
    write_manifest(options.manifest_out, report)
    print("PASS disposable staging Worker, D1, identities and local state cleaned up", flush=True)


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as error:
        print("FAIL " + str(error), file=sys.stderr)
        sys.exit(1)
