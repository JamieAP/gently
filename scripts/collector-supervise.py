#!/usr/bin/env python3
"""Keep the collector and authenticated exporter alive as one foreground job."""
import signal
import json
import os
import subprocess
import sys
import time
import re
from pathlib import Path


def public_setup(gently, check=False):
    """Resolve public CLI configuration without supplying the credential."""
    config_env = dict(os.environ)
    config_env.pop("GENTLY_TOKEN", None)
    try:
        result = subprocess.run([gently, "config", "--json"], env=config_env,
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, timeout=10)
        if result.returncode != 0:
            raise ValueError()
        value = json.loads(result.stdout)
        if set(value) != {"collector_url", "state_dir", "tenant_id", "device_id"} or \
                any(not isinstance(item, str) for item in value.values()) or \
                not Path(value["state_dir"]).is_absolute() or \
                any(not re.fullmatch(r"[A-Za-z0-9_-]{1,64}", value[field]) for field in ["tenant_id", "device_id"]):
            raise ValueError()
        if check:
            checked = subprocess.run([gently, "config", "--check"], env=config_env,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
            if checked.returncode != 0:
                raise ValueError()
        return value
    except (OSError, subprocess.TimeoutExpired, ValueError, TypeError, KeyError):
        raise RuntimeError("Cannot validate Gently setup; run gently config --json and gently config --check for details.") from None


def local_environment(setup):
    environment = dict(os.environ)
    for field in ["tenant_id", "device_id", "state_dir"]:
        environment["GENTLY_" + field.upper()] = setup[field]
    environment["GENTLY_COLLECTOR_URL"] = "http://127.0.0.1:8787"
    return environment


def signal_group(child, value):
    try:
        os.killpg(child.pid, value)
    except ProcessLookupError:
        pass


def group_alive(child):
    try:
        os.killpg(child.pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        # macOS can report EPERM while a signaled group is exiting, before its
        # direct child can be reaped. Keep waiting rather than abort cleanup.
        return True


def stop_children(children):
    # An exited parent can still have live descendants holding ports or pipes.
    for child in children:
        signal_group(child, signal.SIGTERM)
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        for child in children:
            child.poll()
        if not any(group_alive(child) for child in children):
            break
        time.sleep(0.05)
    for child in children:
        signal_group(child, signal.SIGKILL)
        child.wait()


def main():
    if sys.argv[1] == "--check":
        public_setup(sys.argv[2], check=True)
        return 0
    if sys.argv[1] == "--export-only":
        gently = sys.argv[2]
        environment = local_environment(public_setup(gently))
        os.execvpe(gently, [gently, "export", "--watch", "--serve-queries", "--preserve-backlog"], environment)
    node, gently, repo = sys.argv[1:]
    repo = Path(repo)
    environment = local_environment(public_setup(gently))
    children = []
    stopping = False

    def stop(_signal, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    # Only the Worker receives this server credential map. No secret files or
    # command arguments are created; Wrangler disk diagnostics remain disabled.
    worker_env = dict(environment)
    token = worker_env.pop("GENTLY_TOKEN")
    worker_env.pop("GENTLY_RAW_IDENTITY", None)
    worker_env["GENTLY_HOSTS"] = json.dumps([{
        "token": token,
        "tenant_id": worker_env["GENTLY_TENANT_ID"],
        "device_id": worker_env["GENTLY_DEVICE_ID"],
        "capabilities": ["ingest", "read"],
    }])
    try:
        children.append(subprocess.Popen([gently, "export", "--watch", "--serve-queries", "--preserve-backlog"], cwd=repo,
            env=environment, start_new_session=True))
        children.append(subprocess.Popen([
            node, str(repo / "worker/node_modules/wrangler/bin/wrangler.js"), "dev",
            "--config", str(repo / "worker/wrangler.local.toml"), "--local",
            "--ip", "127.0.0.1", "--port", "8787", "--env-file", "/dev/null",
        ], cwd=repo / "worker", env=worker_env, start_new_session=True))
        print("Gently collector and persistent exporter started.", flush=True)
        while not stopping:
            for name, child in zip(("exporter", "collector"), children):
                code = child.poll()
                if code is not None:
                    if code < 0:
                        number = -code
                        try:
                            label = signal.Signals(number).name
                        except ValueError:
                            label = "unknown"
                        reason = f"after signal {label} ({number})"
                        exit_code = 128 + number
                    else:
                        reason = f"with status {code}"
                        exit_code = code
                    print(f"Gently {name} exited {reason}; stopping its companion.",
                          file=sys.stderr, flush=True)
                    return exit_code
            time.sleep(0.25)
        return 0
    finally:
        stop_children(children)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeError as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
