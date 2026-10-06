#!/usr/bin/env python3
"""Keep the collector and authenticated exporter alive as one foreground job."""
import signal
import json
import os
import subprocess
import sys
import time
import re
import stat
from pathlib import Path


def private_collector_state(repo):
    """Protect local D1 metadata as well as ciphertext, without opening DB FDs."""
    root = Path(repo) / "worker" / ".wrangler"
    root.mkdir(mode=0o700, exist_ok=True)

    def harden(path):
        metadata = path.lstat()
        directory = stat.S_ISDIR(metadata.st_mode)
        if metadata.st_uid != os.geteuid() or not (directory or stat.S_ISREG(metadata.st_mode)) or \
                not directory and metadata.st_nlink != 1:
            raise RuntimeError("Local collector state must contain only owned, unlinked regular files and directories.")
        # Path-based chmod does not close another SQLite descriptor and therefore
        # cannot discard the process's POSIX database locks.
        mode = 0o700 if directory else 0o600
        if sys.platform == "linux":
            # Older glibc rejects no-follow chmod. O_PATH pins the inode without
            # creating a regular descriptor whose close would release SQLite locks.
            descriptor = os.open(path, os.O_PATH | os.O_NOFOLLOW | os.O_CLOEXEC)
            try:
                pinned = os.fstat(descriptor)
                if pinned.st_dev != metadata.st_dev or pinned.st_ino != metadata.st_ino:
                    raise RuntimeError("Local collector state changed during permission hardening.")
                os.chmod(f"/proc/self/fd/{descriptor}", mode)
            finally:
                os.close(descriptor)
        else:
            os.chmod(path, mode, follow_symlinks=False)

    harden(root)
    for directory, directories, files in os.walk(root, followlinks=False):
        for name in directories + files:
            harden(Path(directory) / name)


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
    os.umask(0o077)
    if sys.argv[1] == "--check":
        public_setup(sys.argv[2], check=True)
        return 0
    if sys.argv[1] == "--export-only":
        gently = sys.argv[2]
        environment = local_environment(public_setup(gently))
        os.execvpe(gently, [gently, "export", "--watch", "--serve-queries", "--preserve-backlog"], environment)
    node, gently, repo = sys.argv[1:]
    repo = Path(repo)
    private_collector_state(repo)
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
            node, str(repo / "worker/scripts/collector-local.mjs"),
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
