"""Disposable private-terminal tests. No vault, hardware or real credentials."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import subprocess
import sys
import tempfile
import termios
import time

binary = sys.argv[1]
password = "invented-only-for-immediate-private-terminal-tests-é".encode()
flags = termios.ECHO | termios.ECHONL | termios.ICANON | termios.ISIG
input_flags = termios.IXON | termios.IXOFF


def terminal(args, env, exchanges, ignored_signals=(), timeout=20):
    master, slave = pty.openpty()
    original = termios.tcgetattr(master)

    def child_setup():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        for number in ignored_signals:
            signal.signal(number, signal.SIG_IGN)

    child = subprocess.Popen(
        [binary, *map(str, args)], env=env, stdin=slave, stdout=slave,
        stderr=slave, preexec_fn=child_setup,
    )
    os.close(slave)
    transcript = b""
    cursor = answered = 0
    deadline = time.monotonic() + timeout
    echo_was_enabled = False
    try:
        while child.poll() is None:
            if time.monotonic() > deadline:
                raise AssertionError("synthetic terminal command timed out")
            if select.select([master], [], [], 0.1)[0]:
                try:
                    block = os.read(master, 8192)
                except OSError:
                    break
                if not block:
                    break
                transcript += block
            while answered < len(exchanges):
                prompt, response = exchanges[answered]
                found = transcript.find(prompt, cursor)
                if found < 0:
                    break
                cursor = found + len(prompt)
                echo_was_enabled |= bool(termios.tcgetattr(master)[3] & termios.ECHO)
                # Respond immediately: no timing workaround for prompt/ECHO races.
                if isinstance(response, int):
                    os.kill(child.pid, response)
                elif isinstance(response, tuple):
                    if isinstance(response[0], bytes):
                        value, number = response
                        os.write(master, value)
                        # Let the terminal process the flow-control byte before
                        # sending an external signal; prompt input stays immediate.
                        time.sleep(0.05)
                        os.kill(child.pid, number)
                    else:
                        number, value = response
                        os.kill(child.pid, number)
                        os.write(master, value)
                else:
                    os.write(master, response)
                answered += 1
        child.wait(timeout=5)
        while select.select([master], [], [], 0)[0]:
            try:
                block = os.read(master, 8192)
            except OSError:
                break
            if not block:
                break
            transcript += block
        assert not echo_was_enabled, f"private prompt was visible before echo was disabled: {Path(args[-1]).name}"
        for _, response in exchanges:
            if isinstance(response, bytes) and response.strip() and response != b"\x03":
                assert response.strip() not in transcript, "private input was echoed"
        restored = termios.tcgetattr(master)
        assert restored[3] & flags == original[3] & flags, "terminal flags were not restored"
        assert restored[0] & input_flags == original[0] & input_flags, "flow-control flags were not restored"
        assert answered == len(exchanges), f"private prompt contract changed: {Path(args[-1]).name}, exit {child.returncode}"
        return child.returncode, transcript
    finally:
        if child.poll() is None:
            # A broken implementation can block even process exit on a paused
            # Mac terminal. Resume output before terminating the failed fixture.
            try:
                os.write(master, b"\x11")
            except OSError:
                pass
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
        os.close(master)


with tempfile.TemporaryDirectory(prefix="gently-private-terminal-") as directory:
    root = Path(directory)
    env = {"HOME": directory, "PATH": "/usr/bin:/bin"}
    first = b"Reader passphrase: "
    second = b"Confirm reader passphrase: "
    for name, responses in [
        ("mismatch", [(first, password + b"\n"), (second, b"different invented input\n")]),
        ("empty", [(first, b"\n"), (second, b"\n")]),
        ("cancel", [(first, b"\x03")]),
        ("external-term", [(first, signal.SIGTERM)]),
        ("external-int", [(first, signal.SIGINT)]),
        ("external-hup", [(first, signal.SIGHUP)]),
    ]:
        output = root / (name + ".age")
        code, _ = terminal(["raw", "identity", "--out", output], env, responses)
        assert code != 0 and not output.exists(), "failed enrollment created a key"
        if isinstance(responses[0][1], int):
            assert code == -responses[0][1], "external signal termination semantics changed"
    paused = root / "paused-output.age"
    code, _ = terminal(
        ["raw", "identity", "--out", paused], env,
        [(first, (b"\x13", signal.SIGTERM))], timeout=2,
    )
    assert code == -signal.SIGTERM and not paused.exists(), "paused-output cancellation changed"
    ignored = root / "ignored-int.age"
    code, _ = terminal(
        ["raw", "identity", "--out", ignored], env,
        [(first, (signal.SIGINT, b"\n")), (second, b"\n")],
        ignored_signals=(signal.SIGINT,),
    )
    assert code != 0 and not ignored.exists(), "empty ignored-signal enrollment created a key"
    identity = root / "reader.age"
    code, output = terminal(
        ["raw", "identity", "--out", identity], env,
        [(first, password + b"\n"), (second, password + b"\n")],
    )
    assert code == 0, "protected reader creation failed"
    assert identity.read_bytes().startswith(b"age-encryption.org/v1\n")
    assert identity.stat().st_mode & 0o777 == 0o600
    recipient = output.split(b"Recipient: ", 1)[1].splitlines()[0].decode().strip()
    owner = root / "owner.age"
    result = subprocess.run(
        [binary, "raw", "owner-key", "--recipient", recipient, "--out", str(owner)],
        env=env, capture_output=True, timeout=10,
    )
    assert result.returncode == 0, "synthetic owner creation failed"
    unsigned = root / "unsigned.json"
    unsigned.write_text(json.dumps({
        "version": 1, "tenant_id": "personal", "key_epoch": 1,
        "expires_unix_secs": int(time.time()) + 600,
        "readers": [{"device_id": "fixture", "key_id": "fixture-key", "recipient": recipient}],
    }))
    for name, response in [
        ("wrong", b"incorrect invented input\n"), ("empty", b"\n"), ("cancel", b"\x03"),
    ]:
        output = root / (name + ".json")
        code, _ = terminal(
            ["raw", "sign", "--manifest", unsigned, "--owner-key", owner,
             "--identity", identity, "--out", output], env, [(first, response)],
        )
        assert code != 0 and not output.exists(), "failed unlock created signed policy"
    signed = root / "signed.json"
    code, _ = terminal(
        ["raw", "sign", "--manifest", unsigned, "--owner-key", owner,
         "--identity", identity, "--out", signed], env, [(first, password + b"\n")],
    )
    assert code == 0 and signed.exists(), "protected signing failed"
    assert not list(root.glob(".gently-enrollment-*.tmp"))
    print("PASS immediate private input, encrypted enrollment, cancellation and terminal restoration")
