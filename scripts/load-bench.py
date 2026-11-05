#!/usr/bin/env python3
"""Run one disposable load benchmark on a Linux host and record server CPU.

Starts `voice_load_server` pinned to one CPU, runs `voice_load` or `chat_load`
pinned to another (or both generators at once with `--kind mixed`), and samples
server, PostgreSQL and generator CPU over the measured window (from the measured
generator's "Measuring" line for --seconds). Prints one JSON summary line and
keeps the generator reports. See
docs/load-benchmarks.md. Uses a random in-memory benchmark secret. With
TEST_DATABASE_URL exported (the same `_test` database the server uses), it also
records database transactions per second.

    load-bench.py --target DIR --kind voice --users 64 -- --seconds 60
    load-bench.py --target DIR --kind chat --users 128 -- --rate 8 --seconds 60
    load-bench.py --target DIR --kind mixed --server-cpus 0-1 --generator-cpus 0-1         --users 48 --chat-users 256 --chat-args "--rate 4 --seconds 60" --output mixed
"""
import argparse
import json
import os
import resource
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


def postgres_cgroup_seconds():
    """PostgreSQL's service cgroup CPU, including backends that exit mid-run."""
    try:
        with open(f"/proc/{postgres_pids()[0]}/cgroup") as f:
            path = f.read().strip().split("::", 1)[1]
        with open(f"/sys/fs/cgroup{path}/cpu.stat") as f:
            for line in f:
                if line.startswith("usage_usec"):
                    return int(line.split()[1]) / 1e6
    except (OSError, IndexError):
        pass
    return None


def postgres_cpu(pids):
    cgroup = postgres_cgroup_seconds()
    return cgroup if cgroup is not None else cpu_seconds(pids)


def transactions(url):
    """Committed plus rolled-back transactions in the benchmark database."""
    if not url:
        return None
    try:
        out = subprocess.run(
            ["psql", "-XAtq", url, "-c",
             "SELECT xact_commit + xact_rollback FROM pg_stat_database WHERE datname = current_database()"],
            capture_output=True, text=True, timeout=10)
        return int(out.stdout.strip())
    except (OSError, ValueError, subprocess.TimeoutExpired):
        return None


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


def report_fields(path):
    try:
        with open(path) as f:
            report = json.load(f)
    except (OSError, ValueError):
        return {"report": None}
    out = {}
    for key in ("passed", "server_side_passed", "missing_percent", "path_p50_ms", "path_p99_ms",
                "delivery_p50_ms", "delivery_p99_ms", "send_p99_ms", "send_errors",
                "accepted_messages", "scheduled_messages", "failed_clients", "failed_users",
                "failure_reasons", "connected_users", "late", "missing", "generator_skipped"):
        if key in report:
            out[key] = report[key]
    if report.get("video"):
        out["video"] = {k: report["video"][k] for k in (
            "missing_percent", "path_p50_ms", "path_p99_ms", "late", "generator_skipped")}
    return out


class Generator:
    """One generator process; output is echoed to stderr and scanned for "Measuring"."""

    def __init__(self, cmd, env):
        self.measuring = threading.Event()
        self.proc = subprocess.Popen(cmd, env=env, stdout=subprocess.PIPE,
                                     stderr=subprocess.STDOUT, text=True)
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        for line in self.proc.stdout:
            print(line, end="", file=sys.stderr)
            if "Measuring" in line:
                self.measuring.set()
        self.measuring.set()

    def wait(self):
        self.proc.wait()
        self.reader.join()
        return self.proc.returncode


def seconds_of(arguments):
    return float(arguments[arguments.index("--seconds") + 1]) if "--seconds" in arguments else 60.0


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True, help="Cargo target dir with release examples")
    parser.add_argument("--kind", choices=["voice", "chat", "mixed"], required=True)
    parser.add_argument("--users", type=int, required=True,
                        help="generator users; for mixed, the voice users")
    parser.add_argument("--fixture-users", type=int,
                        help="server fixture size (default: the largest user count)")
    parser.add_argument("--server-cpus", default="1", help='"0-1" for the production layout')
    parser.add_argument("--generator-cpus", default="0")
    parser.add_argument("--port", type=int, default=3901)
    parser.add_argument("--output", required=True,
                        help="new generator report path (mixed: a prefix for two reports)")
    parser.add_argument("--chat-users", type=int,
                        help="mixed: chat users; accounts 0..N overlap the voice users")
    parser.add_argument("--chat-args", default="", help="mixed: chat_load arguments")
    parser.add_argument("generator_args", nargs=argparse.REMAINDER,
                        help="voice_load or chat_load arguments (mixed: voice_load)")
    args = parser.parse_args()
    if args.kind == "mixed" and not args.chat_users:
        raise SystemExit("--kind mixed needs --chat-users")
    # Hundreds of users hold three sockets each on both sides; the deployed
    # service runs with LimitNOFILE=65536, so lift the soft limit to the hard one.
    _, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    resource.setrlimit(resource.RLIMIT_NOFILE, (hard, hard))
    extra = [a for a in args.generator_args if a != "--"]
    chat_extra = args.chat_args.split()
    examples = os.path.join(args.target, "release", "examples")
    env = dict(os.environ, THISCORD_LOAD_SECRET=secrets.token_hex(32),
               THISCORD_VOICE_BIND="127.0.0.1:0")
    database = os.environ.get("TEST_DATABASE_URL")
    fixture = args.fixture_users or max(args.users, args.chat_users or 0)
    server_cmd = [
        "taskset", "-c", args.server_cpus, os.path.join(examples, "voice_load_server"),
        "--bind", f"127.0.0.1:{args.port}", "--users", str(fixture),
        "--room-size", "8", "--allow-insecure",
    ]
    server = subprocess.Popen(server_cmd, env=env, stdout=subprocess.DEVNULL,
                              stderr=subprocess.PIPE)
    try:
        wait_port(args.port)
        url = f"http://127.0.0.1:{args.port}"
        voice = [os.path.join(examples, "voice_load"), "--url", url, "--bind", "127.0.0.1:0"]
        chat = [os.path.join(examples, "chat_load"), "--url", url]
        pin = ["taskset", "-c", args.generator_cpus]
        if args.kind == "mixed":
            # Voice joins slowly, so it runs longer: chat starts once voice
            # measures, and the CPU window is chat's measurement with both active.
            seconds = seconds_of(chat_extra)
            voice_args = [a for a in extra]
            if "--seconds" in voice_args:
                del voice_args[voice_args.index("--seconds"):voice_args.index("--seconds") + 2]
            outputs = {"voice": args.output + "-voice.json", "chat": args.output + "-chat.json"}
            first = Generator([*pin, *voice, "--allow-insecure", "--users", str(args.users),
                               "--output", outputs["voice"], *voice_args,
                               "--seconds", str(int(seconds) + 60)], env)
            first.measuring.wait()
            measured = Generator([*pin, *chat, "--allow-insecure", "--users",
                                  str(args.chat_users), "--output", outputs["chat"],
                                  *chat_extra], env)
            generators = [first, measured]
        else:
            seconds = seconds_of(extra)
            outputs = {args.kind: args.output}
            measured = Generator([*pin, *(voice if args.kind == "voice" else chat),
                                  "--allow-insecure", "--users", str(args.users),
                                  "--output", args.output, *extra], env)
            generators = [measured]
        pg = postgres_pids()

        def sample():
            return (time.monotonic(), cpu_seconds([server.pid]), postgres_cpu(pg),
                    cpu_seconds([g.proc.pid for g in generators]), transactions(database))

        measured.measuring.wait()
        start = end = rss = None
        if measured.proc.poll() is None:
            # Generators start sending about one second after "Measuring".
            time.sleep(1)
            start = sample()
            deadline = time.monotonic() + seconds - 1
            while time.monotonic() < deadline and measured.proc.poll() is None:
                time.sleep(0.2)
            end = sample()
            rss = rss_mib(server.pid)
        exits = {name: g.wait() for name, g in zip(outputs, generators)}
        summary = {"kind": args.kind, "users": args.users, "chat_users": args.chat_users,
                   "server_cpus": args.server_cpus, "generator_exit": exits,
                   "generator_args": extra, "chat_args": chat_extra or None,
                   "server_rss_mib": rss or rss_mib(server.pid)}
        if start and end:
            wall = end[0] - start[0]
            summary.update({
                "measured_wall_s": round(wall, 2),
                "server_cpu_percent": round((end[1] - start[1]) / wall * 100, 1),
                "postgres_cpu_percent": round((end[2] - start[2]) / wall * 100, 1),
                # A saturated generator core makes loss/latency a client-side limit.
                "generator_cpu_percent": round((end[3] - start[3]) / wall * 100, 1),
                # Database transactions/s (benchmark database only; needs TEST_DATABASE_URL).
                "postgres_transactions_per_second": None if start[4] is None or end[4] is None
                else round((end[4] - start[4]) / wall, 1),
            })
        if args.kind == "mixed":
            for name, path in outputs.items():
                summary[name] = report_fields(path)
        else:
            summary.update(report_fields(args.output))
        print(json.dumps(summary))
    finally:
        server.send_signal(2)
        try:
            server.wait(timeout=20)
        except subprocess.TimeoutExpired:
            server.kill()


if __name__ == "__main__":
    main()
