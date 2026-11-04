#!/usr/bin/env python3
"""Run one disposable load benchmark on a Linux host and record server CPU.

Starts `voice_load_server` pinned to one CPU, runs `voice_load` or `chat_load`
pinned to another, and samples the server's and PostgreSQL's CPU time over the
generator's measured window (from its "Measuring" line for --seconds). Prints one JSON summary line and keeps the generator report. See
docs/load-benchmarks.md. Uses a random in-memory benchmark secret.

    load-bench.py --target DIR --kind voice --users 64 -- --seconds 60
    load-bench.py --target DIR --kind chat --users 128 -- --rate 8 --seconds 60
"""
import argparse
import json
import os
import secrets
import socket
import subprocess
import sys
import threading
import time

TICKS = os.sysconf("SC_CLK_TCK")


def cpu_seconds(pids):
    total = 0
    for pid in pids:
        try:
            with open(f"/proc/{pid}/stat") as f:
                fields = f.read().rsplit(")", 1)[1].split()
            total += int(fields[11]) + int(fields[12])
        except (FileNotFoundError, ProcessLookupError, IndexError):
            pass
    return total / TICKS


def postgres_pids():
    pids = []
    for entry in os.listdir("/proc"):
        if entry.isdigit():
            try:
                with open(f"/proc/{entry}/comm") as f:
                    if f.read().strip() in ("postgres", "postmaster"):
                        pids.append(int(entry))
            except OSError:
                pass
    return pids


def rss_mib(pid):
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) / 1024
    except OSError:
        pass
    return None


def wait_port(port, timeout=120):
    deadline = time.time() + timeout
    while time.time() < deadline:
        with socket.socket() as s:
            if s.connect_ex(("127.0.0.1", port)) == 0:
                return
        time.sleep(0.2)
    raise SystemExit("benchmark server did not start")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True, help="Cargo target dir with release examples")
    parser.add_argument("--kind", choices=["voice", "chat"], required=True)
    parser.add_argument("--users", type=int, required=True)
    parser.add_argument("--fixture-users", type=int, help="server fixture size (default --users)")
    parser.add_argument("--server-cpus", default="1")
    parser.add_argument("--generator-cpus", default="0")
    parser.add_argument("--port", type=int, default=3901)
    parser.add_argument("--output", required=True, help="new generator report path")
    parser.add_argument("generator_args", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    extra = [a for a in args.generator_args if a != "--"]
    examples = os.path.join(args.target, "release", "examples")
    env = dict(os.environ, THISCORD_LOAD_SECRET=secrets.token_hex(32), THISCORD_VOICE_BIND="127.0.0.1:0")
    server_cmd = [
        "taskset", "-c", args.server_cpus, os.path.join(examples, "voice_load_server"),
        "--bind", f"127.0.0.1:{args.port}", "--users", str(args.fixture_users or args.users),
        "--room-size", "8", "--allow-insecure",
    ]
    server = subprocess.Popen(server_cmd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        wait_port(args.port)
        url = f"http://127.0.0.1:{args.port}"
        if args.kind == "voice":
            gen = [os.path.join(examples, "voice_load"), "--url", url, "--bind", "127.0.0.1:0"]
        else:
            gen = [os.path.join(examples, "chat_load"), "--url", url]
        seconds = float(extra[extra.index("--seconds") + 1]) if "--seconds" in extra else 60.0
        gen = ["taskset", "-c", args.generator_cpus, *gen, "--allow-insecure",
               "--users", str(args.users), "--output", args.output, *extra]
        proc = subprocess.Popen(gen, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        pg = postgres_pids()
        start = None
        samples = {}

        def finish():
            samples["end"] = (time.monotonic(), cpu_seconds([server.pid]), cpu_seconds(pg))
            samples["rss"] = rss_mib(server.pid)

        timer = None
        for line in proc.stdout:
            print(line, end="", file=sys.stderr)
            if start is None and "Measuring" in line:
                # Generators start sending about one second after this line.
                time.sleep(1)
                start = (time.monotonic(), cpu_seconds([server.pid]), cpu_seconds(pg))
                timer = threading.Timer(seconds - 1, finish)
                timer.start()
        proc.wait()
        if timer:
            timer.join()
        end = samples.get("end")
        rss = samples.get("rss", rss_mib(server.pid))
        summary = {"kind": args.kind, "users": args.users, "generator_exit": proc.returncode,
                   "generator_args": extra, "server_rss_mib": rss}
        if start and end:
            wall = end[0] - start[0]
            summary.update({
                "measured_wall_s": round(wall, 2),
                "server_cpu_percent": round((end[1] - start[1]) / wall * 100, 1),
                "postgres_cpu_percent": round((end[2] - start[2]) / wall * 100, 1),
            })
        try:
            with open(args.output) as f:
                report = json.load(f)
            for key in ("passed", "missing_percent", "path_p50_ms", "path_p99_ms", "delivery_p50_ms",
                        "delivery_p99_ms", "send_p99_ms", "send_errors", "accepted_messages",
                        "scheduled_messages", "failed_clients", "failed_users", "late", "missing"):
                if key in report:
                    summary[key] = report[key]
            if report.get("video"):
                summary["video"] = {k: report["video"][k] for k in (
                    "missing_percent", "path_p50_ms", "path_p99_ms", "late", "generator_skipped")}
        except (OSError, ValueError):
            summary["report"] = None
        print(json.dumps(summary))
    finally:
        server.send_signal(2)
        try:
            server.wait(timeout=20)
        except subprocess.TimeoutExpired:
            server.kill()


if __name__ == "__main__":
    main()
