#!/usr/bin/env python3
"""Run load generators on this machine against a benchmark server on another.

The remote host runs `scripts/load-bench.py --kind serve` (the server plus CPU
sampling) over SSH; signaling goes through an SSH tunnel to its loopback port and
media goes directly over UDP to `--media-ip`, so the generators' CPU no longer
competes with the server. The benchmark secret is generated here and sent over
the SSH session's stdin, never in arguments. See docs/load-benchmarks.md.

    load-remote.py --ssh vps --remote-dir thiscord-bench --target target \\
        --media-ip 57.129.178.14 --bind 172.31.185.21 --output run \\
        --voice "--users 128 --seconds 30" --chat "--users 256 --rate 2 --seconds 30"

The measured window is the chat generator's when chat runs, else voice's.
"""
import argparse
import json
import os
import secrets
import shlex
import subprocess
import sys
import threading
import time


class Generator:
    def __init__(self, cmd, env):
        self.measuring = threading.Event()
        # Only a "Measuring" line starts a window; exiting early also sets the event.
        self.started = False
        self.proc = subprocess.Popen(cmd, env=env, stdout=subprocess.PIPE,
                                     stderr=subprocess.STDOUT, text=True)
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.proc.stdout:
            print(line, end="", file=sys.stderr)
            if "Measuring" in line:
                self.started = True
                self.measuring.set()
        self.measuring.set()


def users(arguments):
    return int(arguments[arguments.index("--users") + 1])


def seconds(arguments):
    return float(arguments[arguments.index("--seconds") + 1]) if "--seconds" in arguments else 60.0


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ssh", required=True, help="SSH destination of the server host")
    parser.add_argument("--remote-dir", required=True, help="checkout on the server host (relative to its home)")
    parser.add_argument("--remote-target", default="target", help="Cargo target dir on the server host, relative to --remote-dir")
    parser.add_argument("--target", required=True, help="local Cargo target dir with release examples")
    parser.add_argument("--media-ip", required=True, help="server host's public IP for media")
    parser.add_argument("--bind", required=True, help="local interface IP for generator media")
    parser.add_argument("--server-cpus", default="0-1")
    parser.add_argument("--port", type=int, default=3911)
    parser.add_argument("--output", required=True, help="report prefix")
    parser.add_argument("--voice", default="", help="voice_load arguments (include --users)")
    parser.add_argument("--chat", default="", help="chat_load arguments (include --users)")
    parser.add_argument("--chat-on-server", action="store_true",
                        help="run chat_load on the server host (SSH tunnels do not carry thousands of sockets)")
    args = parser.parse_args()
    voice, chat = shlex.split(args.voice), shlex.split(args.chat)
    if not voice and not chat:
        raise SystemExit("give --voice and/or --chat")
    fixture = max([users(a) for a in (voice, chat) if a])
    secret = secrets.token_hex(32)
    remote = (f"cd {shlex.quote(args.remote_dir)} && export $(grep ^TEST_DATABASE_URL= backend/.env) && "
              f"exec python3 scripts/load-bench.py --kind serve --target {shlex.quote(args.remote_target)} "
              f"--users {fixture} --server-cpus {args.server_cpus} --port {args.port} "
              f"--media-bind {args.media_ip}:0 --output unused")
    server = subprocess.Popen(
        ["ssh", "-o", "BatchMode=yes", "-o", "ExitOnForwardFailure=yes",
         "-L", f"{args.port}:127.0.0.1:{args.port}", args.ssh, remote],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    server.stdin.write(secret + "\n")
    server.stdin.flush()
    if server.stdout.readline().strip() != "READY":
        raise SystemExit("remote benchmark server did not start")
    env = dict(os.environ, THISCORD_LOAD_SECRET=secret)
    url = f"http://127.0.0.1:{args.port}"
    examples = os.path.join(args.target, "release", "examples")
    generators, outputs = [], {}
    try:
        if voice:
            outputs["voice"] = args.output + "-voice.json"
            if chat:
                # Voice joins slowly: keep it running through chat's whole window.
                voice = [a for a in voice]
                if "--seconds" in voice:
                    del voice[voice.index("--seconds"):voice.index("--seconds") + 2]
                voice += ["--seconds", str(int(seconds(chat)) + 60)]
            generators.append(Generator([
                os.path.join(examples, "voice_load"), "--url", url, "--bind", f"{args.bind}:0",
                "--allow-insecure", "--output", outputs["voice"], *voice], env))
            if chat:
                generators[-1].measuring.wait()
        summary = {"voice_args": voice or None, "chat_args": chat or None, "off_host": True,
                   "chat_on_server": args.chat_on_server}
        if chat and args.chat_on_server:
            server.stdin.write("chat " + " ".join(chat) + "\n")
            server.stdin.flush()
            summary.update(json.loads(server.stdout.readline()))
            measured = None
        elif chat:
            outputs["chat"] = args.output + "-chat.json"
            generators.append(Generator([
                os.path.join(examples, "chat_load"), "--url", url, "--allow-insecure",
                "--output", outputs["chat"], *chat], env))
        if not (chat and args.chat_on_server):
            measured = generators[-1]
            measured.measuring.wait()
        window = seconds(chat if chat else voice)
        if measured is not None and measured.started and measured.proc.poll() is None:
            time.sleep(1)
            server.stdin.write("start\n")
            server.stdin.flush()
            deadline = time.monotonic() + window - 1
            while time.monotonic() < deadline and measured.proc.poll() is None:
                time.sleep(0.2)
            server.stdin.write("stop\n")
            server.stdin.flush()
            summary.update(json.loads(server.stdout.readline()))
        for name, g in zip(outputs, generators):
            g.proc.wait()
            try:
                with open(outputs[name]) as f:
                    report = json.load(f)
            except (OSError, ValueError):
                summary[name] = {"report": None}
                continue
            keys = ("passed", "server_side_passed", "missing_percent", "late", "missing",
                    "path_p50_ms", "path_p99_ms", "generator_skipped", "failed_clients",
                    "delivery_p50_ms", "delivery_p99_ms", "send_p99_ms", "send_errors",
                    "failed_users", "failure_reasons", "connected_users", "connect_seconds",
                    "expected_deliveries")
            summary[name] = {k: report[k] for k in keys if k in report}
            if report.get("video"):
                summary[name]["video"] = {k: report["video"][k] for k in (
                    "missing_percent", "late", "path_p99_ms", "expected_deliveries")}
        print(json.dumps(summary))
    finally:
        for g in generators:
            if g.proc.poll() is None:
                g.proc.terminate()
        try:
            server.stdin.close()
        except OSError:
            pass
        server.wait(timeout=60)


if __name__ == "__main__":
    main()
