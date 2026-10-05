#!/usr/bin/env python3
"""Keep the collector and authenticated exporter alive as one foreground job."""
import signal
import subprocess
import sys
import time
from pathlib import Path


def main():
    node, gently, repo = sys.argv[1:]
    repo = Path(repo)
    children = []
    stopping = False

    def stop(_signal, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    try:
        children.append(subprocess.Popen([gently, "export", "--watch"], cwd=repo))
        children.append(subprocess.Popen([
            node, str(repo / "worker/node_modules/wrangler/bin/wrangler.js"), "dev",
            "--config", str(repo / "worker/wrangler.local.toml"), "--local",
            "--ip", "127.0.0.1", "--port", "8787", "--env-file", "/dev/null",
        ], cwd=repo / "worker"))
        print("Gently collector and persistent exporter started.", flush=True)
        while not stopping:
            for child in children:
                code = child.poll()
                if code is not None:
                    print("A Gently service exited; stopping its companion.", file=sys.stderr)
                    return code
            time.sleep(0.25)
        return 0
    finally:
        for child in children:
            if child.poll() is None:
                child.terminate()
        for child in children:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


if __name__ == "__main__":
    sys.exit(main())
