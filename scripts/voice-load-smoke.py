"""Functional loopback checks; not a capacity measurement. No third-party modules."""
import json
import os
from pathlib import Path
import queue
import secrets
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request


def main():
    server, client = sys.argv[1:]
    env = os.environ.copy()
    for key in ("THISCORD_STUN_URL", "THISCORD_TURN_URL", "THISCORD_TURN_SECRET"):
        env.pop(key, None)
    env["THISCORD_VOICE_BIND"] = "127.0.0.1:0"
    env["THISCORD_LOAD_SECRET"] = secrets.token_hex(32)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    url = f"http://127.0.0.1:{port}"
    with tempfile.TemporaryDirectory(prefix="thiscord-voice-load-") as directory:
        host = subprocess.Popen(
            [server, "--bind", f"127.0.0.1:{port}", "--users", "16", "--allow-insecure"],
            env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
        )
        try:
            for _ in range(100):
                if host.poll() is not None:
                    raise RuntimeError("Benchmark server exited before readiness")
                try:
                    with urllib.request.urlopen(url + "/api/v1/ready", timeout=1) as response:
                        if response.status == 200:
                            break
                except OSError:
                    time.sleep(0.1)
            else:
                raise RuntimeError("Benchmark server readiness timeout")

            def run(name, users, secret=None):
                output = Path(directory) / (name + ".json")
                child_env = env.copy()
                if secret is not None:
                    child_env["THISCORD_LOAD_SECRET"] = secret
                result = subprocess.run(
                    [client, "--url", url, "--allow-insecure", "--users", str(users),
                     "--seconds", "3", "--output", str(output)],
                    env=child_env, text=True, capture_output=True, timeout=65,
                )
                # The executables emit only sanitized stage errors and counters.
                print(result.stdout, end="")
                print(result.stderr, end="", file=sys.stderr)
                return result.returncode, json.loads(output.read_text())

            for users in (8, 16):
                code, report = run(f"users-{users}", users)
                # Functional CI can be noisy: verify delivery and accounting without
                # asserting a machine-performance PASS threshold.
                assert code in (0, 2) and report["complete"]
                assert report["failed_clients"] == 0
                assert report["invalid_or_cross_room"] == 0
                assert len(report["streams"]) == users * 7
                assert report["expected_deliveries"] == report["sent"] * 7
                assert all(s["received_unique"] > 0 for s in report["streams"])
                time.sleep(0.3)
            code, report = run("wrong-secret", 8, secrets.token_hex(32))
            assert code == 2 and not report["complete"] and not report["passed"]
            assert report["sent"] == 0
            # More clients than the explicitly provisioned fixture must fail closed.
            code, report = run("oversubscribed", 24)
            assert code == 2 and not report["complete"] and not report["passed"]
            time.sleep(0.3)
            output = Path(directory) / "cancelled.json"
            interrupted = subprocess.Popen(
                [client, "--url", url, "--allow-insecure", "--users", "8",
                 "--seconds", "60", "--output", str(output)],
                env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
            )
            lines = queue.Queue()

            def drain():
                for line in interrupted.stdout:
                    lines.put(line)
                lines.put(None)

            reader = threading.Thread(target=drain, daemon=True)
            reader.start()
            try:
                while True:
                    line = lines.get(timeout=45)
                    if line is None:
                        raise RuntimeError("Cancellation client exited before measurement")
                    if "All media paths verified" in line:
                        break
                interrupted.send_signal(signal.SIGINT)
                assert interrupted.wait(timeout=10) == 2
                cancelled = json.loads(output.read_text())
                assert not cancelled["complete"] and not cancelled["passed"]
            finally:
                if interrupted.poll() is None:
                    interrupted.kill()
                    interrupted.wait()
                reader.join(timeout=2)
                interrupted.stdout.close()
            time.sleep(0.3)
            _, report = run("after-cancellation", 8)
            assert report["complete"] and report["failed_clients"] == 0
            print("Voice load smoke checks passed (delivery, isolation, reconnect cleanup, rejected credentials/capacity).")
        finally:
            if host.poll() is None:
                host.send_signal(signal.SIGINT)
            try:
                text, _ = host.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                host.kill()
                text, _ = host.communicate()
                raise RuntimeError("Benchmark server failed to shut down") from None
            print(text, end="")
            if host.returncode != 0:
                raise RuntimeError("Benchmark server failed")


if __name__ == "__main__":
    main()
