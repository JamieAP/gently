#!/usr/bin/env python3
"""Native coding-agent release evidence in isolated state, synthetic only.

Runs one real coding-agent session (Claude Code or Codex, CLI or desktop coding
surface) against a disposable Gently state, an enrolled software reader and an
ephemeral local Wrangler/D1 collector, then verifies native hook receipts, tool
capture, an MCP query made by the agent itself, keyless encrypted export,
enrolled-reader decryption, tenant rejection and plaintext canary absence.

Agent configuration lives only in a temporary project: Claude Code loads project
settings and an explicit MCP config; Codex ignores the user config and receives
hooks and MCP through `-c`. The user's own agent configuration is not read or
written, and the agents keep their existing sign-in. For a desktop surface the
runner prints the project and prompt, then waits while the operator runs it.
Output contains phase names, versions and counts only.
"""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import secrets
import shlex
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import time
import tomllib

REPO = Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("acceptance_local", REPO / "scripts/acceptance-local.py")
acceptance = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(acceptance)
require, phase, command, private_terminal, http, stop_group = (
    acceptance.require, acceptance.phase, acceptance.command, acceptance.private_terminal,
    acceptance.http, acceptance.stop_group)

INGEST, READ, OTHER_READ = "invented-native-ingest", "invented-native-read", "invented-native-other-read"
REQUIRED_EVENTS = {"SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop"}


def toml_inline(value):
    if isinstance(value, dict):
        return "{" + ", ".join(f"{json.dumps(k)} = {toml_inline(v)}" for k, v in value.items()) + "}"
    if isinstance(value, list):
        return "[" + ", ".join(toml_inline(item) for item in value) + "]"
    if isinstance(value, bool):
        return "true" if value else "false"
    return json.dumps(value)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--harness", choices=["claude", "codex"], required=True)
    parser.add_argument("--surface", choices=["cli", "desktop"], required=True)
    parser.add_argument("--binary", type=Path, required=True, help="release-candidate gently binary")
    parser.add_argument("--agent", help="agent CLI (default: claude or codex on PATH)")
    options = parser.parse_args()
    binary = options.binary.resolve()
    node = shutil.which("node")
    wrangler = REPO / "worker/node_modules/wrangler/bin/wrangler.js"
    agent = options.agent or shutil.which(options.harness)
    require(binary.is_file() and node and wrangler.is_file(), "build gently and install Worker dependencies first")
    require(options.surface == "desktop" or agent, "agent CLI not found")
    gently_version = command([binary, "--version"], os.environ.copy()).stdout.decode().strip()
    agent_version = (command([agent, "--version"], os.environ.copy()).stdout.decode().strip()
                     if agent else "operator-run desktop app")
    canary = "invented-native-canary-" + secrets.token_hex(8)

    with tempfile.TemporaryDirectory(prefix="gently-native-") as temporary:
        root = Path(temporary).resolve()
        home = root / "home"
        home.mkdir()
        project = root / "project"
        project.mkdir()
        command(["git", "init", "-q"], os.environ.copy(), cwd=project)
        (project / "README.md").write_text("Synthetic Gently release-check project.\n")
        env = {"HOME": str(home), "XDG_CONFIG_HOME": str(home / "config"),
               "PATH": os.pathsep.join([str(Path(node).parent), "/usr/bin", "/bin", "/usr/sbin", "/sbin"]),
               "WRANGLER_SEND_METRICS": "false", "WRANGLER_WRITE_LOGS": "false", "WRANGLER_LOG": "error",
               "NO_COLOR": "1"}
        principals = [
            {"token": INGEST, "tenant_id": "lab", "device_id": "native", "capabilities": ["ingest"]},
            {"token": READ, "tenant_id": "lab", "device_id": "reader", "capabilities": ["read"]},
            {"token": OTHER_READ, "tenant_id": "work", "device_id": "reader", "capabilities": ["read"]},
        ]
        worker_env = {**env, "GENTLY_HOSTS": json.dumps(principals)}
        d1 = [node, wrangler, "d1", "execute", "gently", "--local", "--config",
              REPO / "worker/wrangler.local.toml", "--persist-to", root / "d1"]
        command([*d1, "--file", REPO / "worker/schema.sql"], worker_env, REPO / "worker")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        log_path = root / "worker.log"
        log = log_path.open("wb")
        worker = subprocess.Popen([node, str(wrangler), "dev", "--config", str(REPO / "worker/wrangler.local.toml"),
                                   "--local", "--ip", "127.0.0.1", "--port", str(port), "--inspector-port", "0",
                                   "--persist-to", str(root / "d1"), "--env-file", "/dev/null"],
                                  env=worker_env, cwd=REPO / "worker", stdout=log, stderr=log, start_new_session=True)
        watcher = None
        try:
            deadline = time.monotonic() + 45
            while True:
                require(worker.poll() is None, "Wrangler exited before readiness")
                try:
                    if http(base, READ, "/v1/query?tenant_id=lab&op=traces")[0] == 200:
                        break
                except OSError:
                    pass
                require(time.monotonic() < deadline, "Wrangler readiness timed out")
                time.sleep(0.1)
            states = {}
            for label, device in [("native", "native"), ("reader", "reader")]:
                directory = root / label
                directory.mkdir()
                (directory / "config.toml").write_text(
                    f'collector_url = "{base}"\nprefer_quic = false\ntenant_id = "lab"\n'
                    f'device_id = "{device}"\nexport_timeout_secs = 2\nquery_timeout_secs = 5\n')
                states[label] = {**env, "GENTLY_STATE_DIR": str(directory)}
            phase(f"ephemeral Wrangler/D1 collector and isolated state for {gently_version}")

            identity = root / "reader.age"
            output = private_terminal([binary, "raw", "identity", "--out", identity], states["reader"],
                                      ["Reader passphrase: ", "Confirm reader passphrase: "])
            recipient = output.split("Recipient: ", 1)[1].splitlines()[0].strip()
            owner = root / "owner.age"
            owner_public = command([binary, "raw", "owner-key", "--recipient", recipient, "--out", owner],
                                   states["reader"]).stdout.decode().split("Owner public key: ", 1)[1].strip()
            unsigned, manifest, trust = root / "unsigned.json", root / "manifest.json", root / "trust.json"
            unsigned.write_text(json.dumps({"version": 1, "tenant_id": "lab", "key_epoch": 1,
                                            "expires_unix_secs": int(time.time()) + 3600,
                                            "readers": [{"device_id": "reader", "key_id": "key0",
                                                         "recipient": recipient}]}))
            private_terminal([binary, "raw", "sign", "--manifest", unsigned, "--owner-key", owner,
                              "--identity", identity, "--out", manifest], states["reader"], ["Reader passphrase: "])
            command([binary, "raw", "trust", "--manifest", manifest, "--owner-public", owner_public,
                     "--out", trust], states["native"])
            phase("enrolled software reader and signed, pinned recipient policy")

            hook_env = {"GENTLY_STATE_DIR": states["native"]["GENTLY_STATE_DIR"], "GENTLY_CAPTURE_RAW_VALUES": "1",
                        "GENTLY_RAW_MANIFEST": str(manifest), "GENTLY_RAW_TRUST": str(trust)}
            prefix = "/usr/bin/env " + " ".join(f"{k}={shlex.quote(v)}" for k, v in hook_env.items()) + " "
            mcp_env = {"GENTLY_STATE_DIR": states["reader"]["GENTLY_STATE_DIR"], "GENTLY_TOKEN": READ}
            init_home = root / "init-home"
            init_home.mkdir()
            command([binary, "init", "--" + options.harness],
                    {**env, "HOME": str(init_home), "GENTLY_STATE_DIR": hook_env["GENTLY_STATE_DIR"]})

            def isolate(hooks):
                count = 0
                for groups in hooks.values():
                    for group in groups:
                        for handler in group["hooks"]:
                            require(str(binary) in handler["command"], "unexpected hook command")
                            handler["command"] = prefix + handler["command"]
                            count += 1
                return count

            agent_env = {k: v for k, v in os.environ.items() if not k.startswith(("CLAUDECODE", "CLAUDE_CODE_"))}
            if options.harness == "claude":
                hooks = json.loads((init_home / ".claude/settings.json").read_text())["hooks"]
                registered = isolate(hooks)
                (project / ".claude").mkdir()
                (project / ".claude/settings.json").write_text(json.dumps({"hooks": hooks}, indent=2))
                (project / ".mcp.json").write_text(json.dumps({"mcpServers": {"gently": {
                    "type": "stdio", "command": str(binary), "args": ["mcp"], "env": mcp_env}}}, indent=2))
                run = [agent, "-p", None, "--setting-sources", "project", "--mcp-config", str(project / ".mcp.json"),
                       "--strict-mcp-config", "--allowedTools", "Bash(ls)", "mcp__gently__list_traces",
                       "--output-format", "json"]
            else:
                config = tomllib.loads((init_home / ".codex/config.toml").read_text())
                hooks = config["hooks"]
                registered = isolate(hooks)
                server = {"command": str(binary), "args": ["mcp"], "env": mcp_env}
                (project / ".codex").mkdir()
                (project / ".codex/config.toml").write_text(
                    "".join(f"[[hooks.{event}]]\n" + "".join(
                        f"[[hooks.{event}.hooks]]\ntype = \"command\"\ncommand = {json.dumps(h['command'])}\n"
                        for h in group["hooks"]) for event, groups in hooks.items() for group in groups) +
                    "[mcp_servers.gently]\n" + "".join(f"{k} = {toml_inline(v)}\n" for k, v in server.items()))
                run = [agent, "exec", "--ignore-user-config", "--skip-git-repo-check", "--dangerously-bypass-hook-trust",
                       "--sandbox", "read-only", "-c", 'approval_policy="never"', "-c", "hooks=" + toml_inline(hooks),
                       "-c", "mcp_servers.gently=" + toml_inline(server), "-C", str(project), None]
            phase(f"{registered} {options.harness} hook registrations isolated to this project, plus the gently MCP server")

            export_env = {**states["native"], "GENTLY_TOKEN": INGEST, "GENTLY_SYNC_RAW_VALUES": "1"}
            watcher = subprocess.Popen([binary, "export", "--watch"], env=export_env, stdout=subprocess.DEVNULL,
                                       stderr=subprocess.DEVNULL, start_new_session=True)
            prompt = ("This is a synthetic Gently release check. Run the shell command `ls` once in this directory, "
                      "then call the gently MCP tool list_traces once. Reply with the single word done and do not "
                      f"repeat this message. Reference {canary}.")
            if options.surface == "cli":
                run[run.index(None)] = prompt
                result = subprocess.run([str(a) for a in run], cwd=project, env=agent_env, capture_output=True,
                                        stdin=subprocess.DEVNULL, timeout=600)
                require(result.returncode == 0, f"{options.harness} session failed (output withheld)")
            else:
                prompt_file = root / "PROMPT.txt"
                prompt_file.write_text(prompt + "\n")
                print(f"Open {project} in the {options.harness} desktop coding surface and send the prompt in "
                      f"{prompt_file}. Approve the project's gently hooks and MCP server if asked. When the agent "
                      "has replied, press Enter here.", flush=True)
                input()
            phase(f"{options.harness} {options.surface} session ({agent_version}) completed")

            db_path = Path(hook_env["GENTLY_STATE_DIR"]) / "tenants/lab/devices/native/state.db"
            deadline = time.monotonic() + 60
            while True:
                with sqlite3.connect(f"file:{db_path}?mode=ro", uri=True) as db:
                    queued = db.execute("SELECT count(*) FROM outbox").fetchone()[0]
                    raw_total, raw_unsynced = db.execute(
                        "SELECT count(*), coalesce(sum(synced = 0), 0) FROM raw_objects").fetchone()
                if queued == 0 and raw_unsynced == 0:
                    break
                require(time.monotonic() < deadline, "watcher did not drain the native capture")
                time.sleep(0.5)
            stop_group(watcher)
            watcher = None
            require(raw_total > 0, "no encrypted raw values were captured")
            phase(f"keyless watcher exported all metadata and {raw_total} encrypted raw values")

            status, spans = http(base, READ, "/v1/query?tenant_id=lab&op=spans&limit=1000")
            require(status == 200 and spans, "collector returned no native spans")
            receipts = {span["name"].split(":", 1)[1] for span in spans if span.get("name", "").startswith("hook:")}
            missing = REQUIRED_EVENTS - receipts
            require(not missing, "missing native hook receipts: " + ", ".join(sorted(missing)))
            tool_spans = sorted({span["name"] for span in spans if "list_traces" in span.get("name", "")
                                 and not span["name"].startswith("hook:")})
            require(tool_spans, "the agent's own gently MCP call was not captured as a tool span")
            sessions = sorted({span.get("session_id") for span in spans if span.get("session_id")})
            require(len(sessions) == 1, "expected exactly one native session")
            phase(f"native hook receipts {', '.join(sorted(receipts))}; MCP tool span {', '.join(tool_spans)}")

            require(http(base, OTHER_READ, "/v1/query?tenant_id=lab&op=spans")[0] == 403 and
                    http(base, INGEST, "/v1/query?tenant_id=lab&op=spans")[0] == 403,
                    "tenant or capability separation failed")
            decrypted = private_terminal([binary, "spans", "--session-id", sessions[0], "--json"],
                                         {**states["reader"], "GENTLY_TOKEN": READ, "GENTLY_RESOLVE_RAW_VALUES": "1",
                                          "GENTLY_RAW_IDENTITY": str(identity)}, ["Reader passphrase: "])
            require(canary in decrypted, "enrolled reader could not decrypt the native prompt")
            phase("tenant rejection and enrolled-reader decryption of the native prompt")

            log.flush()
            for directory in (root / "native", root / "reader", root / "d1"):
                for path in directory.rglob("*"):
                    if path.is_file():
                        require(canary.encode() not in path.read_bytes(), "plaintext canary in persistent state")
            require(canary.encode() not in log_path.read_bytes(), "plaintext canary in collector output")
            phase("no plaintext canary in local SQLite/WAL, D1 or collector output")
        finally:
            if watcher is not None:
                stop_group(watcher)
            stop_group(worker)
            log.close()
    phase(f"{options.harness} {options.surface}: {agent_version}; {gently_version}; disposable state removed")


if __name__ == "__main__":
    main()
