#!/usr/bin/env python3
"""Linux CPU profiles of disposable SyntraDB soak workers, with bounded waits."""
import argparse
from collections import Counter
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time

ACTORS = frozenset([
    "writer_0", "writer_1", "read_committed", "repeatable_read", "applier_0",
    "applier_1", "pruner", "keyed_monitor", "vacuum",
])
ACTIVITY = """SELECT coalesce(json_agg(row_to_json(a)),'[]'::json) FROM (
    SELECT pid,application_name,state,wait_event_type,wait_event,
           backend_xmin::text AS backend_xmin,xact_start,query_start,left(query,2048) AS query
    FROM pg_stat_activity WHERE application_name IN
        ('writer_0','writer_1','read_committed','repeatable_read','applier_0',
         'applier_1','pruner','keyed_monitor','vacuum')
    ORDER BY application_name LIMIT 16
) AS a"""
STOP = threading.Event()


def schedule(seconds):
    plans = {240: [1, 3], 1200: [2, 8, 12, 18], 7200: [8, 28, 58, 65]}
    if seconds not in plans:
        raise ValueError("profile duration must be 240, 1200 or 7200 seconds")
    return plans[seconds]


def actors(rows, complete=True):
    if not isinstance(rows, list) or len(rows) > len(ACTORS):
        raise ValueError("invalid worker activity result")
    result = {}
    for row in rows:
        if not isinstance(row, dict):
            raise ValueError("worker activity rows must be objects")
        name, pid = row.get("application_name"), row.get("pid")
        if name not in ACTORS or name in result or type(pid) is not int or pid <= 0:
            raise ValueError("invalid or duplicate soak worker")
        result[name] = pid
    if len(set(result.values())) != len(result):
        raise ValueError("one backend was assigned multiple actor names")
    if complete and set(result) != ACTORS:
        raise ValueError("profile requires all nine soak workers")
    return result


def now():
    return datetime.now(timezone.utc).isoformat()


def write_json(path, value):
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as out:
        temporary = Path(out.name)
        try:
            out.write(json.dumps(value, indent=2) + "\n")
            out.flush()
            os.fsync(out.fileno())
            os.replace(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)


def command(argv, **kwargs):
    return subprocess.run(argv, check=True, timeout=120, **kwargs)


class Perf:
    def __init__(self, binary, sudo=False):
        self.binary = str(binary)
        self.sudo = sudo

    def record_command(self, data, pids, seconds):
        if not pids or len(pids) > 9 or any(type(p) is not int or p <= 0 for p in pids):
            raise ValueError("invalid profile PID list")
        if type(seconds) is not int or not 1 <= seconds <= 30:
            raise ValueError("profile window must be 1..30 seconds")
        prefix = ["sudo", "-n"] if self.sudo else []
        return prefix + [self.binary, "record", "-e", "cpu-clock:u", "-F", "49",
                         "--call-graph", "dwarf,16384", "-p", ",".join(map(str, pids)),
                         "-o", str(data), "--", "sleep", str(seconds)]

    def stop_record(self, process):
        if self.sudo:
            command(["sudo", "-n", "kill", "-INT", "--", str(-process.pid)],
                    capture_output=True, text=True)
        else:
            os.killpg(process.pid, signal.SIGINT)
        process.wait(timeout=15)

    def record(self, directory, pids, seconds, observe=None):
        directory.mkdir(parents=True, exist_ok=False)
        data = directory / "perf.data"
        argv = self.record_command(data, pids, seconds)
        write_json(directory / "command.json", argv)
        started = time.monotonic()
        with (directory / "record.stdout").open("w") as out, (directory / "record.stderr").open("w") as err:
            process = subprocess.Popen(argv, stdout=out, stderr=err, start_new_session=True)
            try:
                while process.poll() is None:
                    if STOP.is_set():
                        raise InterruptedError("profile collection interrupted")
                    if time.monotonic() - started > seconds + 20:
                        raise TimeoutError("perf record exceeded its bounded window")
                    if observe is not None:
                        observe()
                    STOP.wait(1)
                if process.returncode != 0:
                    raise RuntimeError("perf record failed; inspect record.stderr")
            finally:
                try:
                    if process.poll() is None:
                        self.stop_record(process)
                finally:
                    if self.sudo and data.is_file():
                        command(["sudo", "-n", "chown", "--", f"{os.getuid()}:{os.getgid()}", str(data)],
                                capture_output=True, text=True)
        if not data.is_file() or data.stat().st_size == 0:
            raise RuntimeError("perf produced no data file")
        return data

    def samples(self, data, output, pids):
        with output.open("w") as out, output.with_suffix(".stderr").open("w") as err:
            command([self.binary, "script", "-i", str(data), "-F", "pid"], stdout=out, stderr=err)
        wanted = {str(pid) for pid in pids}
        return Counter(line.strip() for line in output.read_text().splitlines()
                       if line.strip() in wanted)

    def reports(self, data, directory):
        for label, options in [("self", ["--no-children"]), ("callchains", [])]:
            with (directory / f"{label}.txt").open("w") as out, (directory / f"{label}.stderr").open("w") as err:
                command([self.binary, "report", "-i", str(data), "--stdio", "-n",
                         "--percent-limit", "0.1", "--sort", "pid,dso,symbol"] + options,
                        stdout=out, stderr=err)


def probe(perf, output):
    # A fixed, disposable CPU workload verifies actual sample delivery before
    # the expensive PostgreSQL build. No benchmark or customer process is used.
    busy = subprocess.Popen([sys.executable, "-c",
        "import time\nend=time.monotonic()+8\nwhile time.monotonic()<end: pass\n"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        data = perf.record(output, [busy.pid], 2)
        counts = perf.samples(data, output / "sample-pids.txt", [busy.pid])
        if counts[str(busy.pid)] < 5:
            raise RuntimeError("profiler preflight did not deliver CPU samples")
        perf.reports(data, output)
        write_json(output / "probe.json", {"status": "passed", "pid": busy.pid,
                   "samples": counts[str(busy.pid)], "event": "cpu-clock:u",
                   "call_graph": "dwarf,16384", "frequency_hz": 49})
    finally:
        if busy.poll() is None:
            busy.terminate()
        busy.wait(timeout=10)


def query(pg_bin, socket):
    environment = dict(os.environ, PGCONNECT_TIMEOUT="2", PGAPPNAME="syntra_profile_observer",
                       PGOPTIONS="-c statement_timeout=2000 -c lock_timeout=1000", LC_ALL="C")
    result = subprocess.run([str(pg_bin / "psql"), "-XAtq", "-v", "ON_ERROR_STOP=1",
                             "-h", str(socket), "-p", "28918", "-U", "syntra_bench",
                             "-d", "syntra_enabled", "-c", ACTIVITY],
                            env=environment, capture_output=True, text=True, check=True, timeout=5)
    rows = json.loads(result.stdout)
    actors(rows, complete=False)
    return rows


def discover(pg_bin, timeout):
    deadline = time.monotonic() + timeout
    attempts, last_error = 0, None
    while time.monotonic() < deadline:
        if STOP.is_set():
            raise InterruptedError("profile discovery interrupted")
        matches = []
        for socket in sorted(Path("/tmp").glob("syntra-bench-*"))[:16]:
            if socket.is_symlink() or socket.stat().st_uid != os.getuid():
                continue
            if not (socket / ".s.PGSQL.28918").is_socket():
                continue
            attempts += 1
            try:
                rows = query(pg_bin, socket)
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
                # Startup and recovery temporarily remove the socket/database.
                # Keep the failure count/context and report it on timeout.
                last_error = str(error)[:2000]
                continue
            if len(rows) == len(ACTORS):
                matches.append((socket, actors(rows)))
        if len(matches) > 1:
            raise RuntimeError("multiple active soak clusters; refusing ambiguous profiling")
        if matches:
            return matches[0], {"connection_attempts": attempts, "last_startup_error": last_error}
        STOP.wait(2)
    raise TimeoutError(f"soak workers not found within {timeout}s; last startup error: {last_error}")


def collect(perf, pg_bin, output, seconds, startup_timeout):
    minutes = schedule(seconds)
    output.mkdir(parents=True, exist_ok=False)
    status = {"status": "running", "started_at": now(), "soak_seconds": seconds,
              "scheduled_minutes": minutes, "completed_windows": [],
              "start_reference": "first observation of all nine named workers; within discovery polling delay"}
    try:
        probe(perf, output / "probe")
        (socket, pids), discovery = discover(pg_bin, startup_timeout)
        start = time.monotonic()
        status.update({"workers_seen_at": now(), "socket": str(socket), "actors": pids,
                       "discovery": discovery})
        write_json(output / "collection.json", status)
        for minute in minutes:
            while time.monotonic() - start < minute * 60:
                if STOP.wait(max(0, min(1, minute * 60 - (time.monotonic() - start)))):
                    raise InterruptedError("profile schedule interrupted")
            window = output / f"minute-{minute:03}"
            records = []
            def observe():
                rows = query(pg_bin, socket)
                if actors(rows) != pids:
                    raise RuntimeError("soak worker PIDs changed during profiling")
                records.append({"at": now(), "elapsed_seconds": time.monotonic() - start,
                                "activity": rows})
            print(f"Profiling soak minute {minute}", flush=True)
            try:
                perf.record(window, list(pids.values()), 15, observe)
            finally:
                if window.is_dir():
                    write_json(window / "activity.json", records)
            write_json(window / "window.json", {"minute": minute, "actors": pids,
                       "completed_at": now(), "recorded_seconds": 15,
                       "event": "cpu-clock:u", "frequency_hz": 49, "call_graph": "dwarf,16384"})
            status["completed_windows"].append(minute)
            write_json(output / "collection.json", status)
        status["status"] = "complete"
    except Exception as error:
        status.update({"status": "failed", "error": str(error)})
        raise
    finally:
        status["finished_at"] = now()
        write_json(output / "collection.json", status)


def analyze(perf, output):
    status = json.loads((output / "collection.json").read_text())
    if status["completed_windows"] != schedule(status["soak_seconds"]):
        raise RuntimeError("profile collection has missing or unexpected windows")
    for minute in status["completed_windows"]:
        window = output / f"minute-{minute:03}"
        metadata = json.loads((window / "window.json").read_text())
        counts = perf.samples(window / "perf.data", window / "sample-pids.txt", metadata["actors"].values())
        if not counts:
            raise RuntimeError(f"minute {minute} contains no worker CPU samples")
        perf.reports(window / "perf.data", window)
        write_json(window / "sample-counts.json", {name: counts[str(pid)] for name, pid in metadata["actors"].items()})
    if status["status"] != "complete":
        raise RuntimeError("profile collection was incomplete; partial data was retained")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["probe", "collect", "analyze"])
    parser.add_argument("--perf-bin", required=True, type=Path)
    parser.add_argument("--sudo", action="store_true")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--pg-bin", type=Path)
    parser.add_argument("--soak-seconds", type=int, default=7200)
    parser.add_argument("--startup-timeout", type=int, default=2700)
    args = parser.parse_args()
    if sys.platform != "linux" or not args.perf_bin.is_file():
        parser.error("profiling requires Linux and an installed perf executable")
    if not 1 <= args.startup_timeout <= 2700:
        parser.error("startup timeout must be 1..2700 seconds")
    signal.signal(signal.SIGTERM, lambda *_: STOP.set())
    signal.signal(signal.SIGINT, lambda *_: STOP.set())
    perf = Perf(args.perf_bin, args.sudo)
    if args.mode == "probe":
        probe(perf, args.output)
    elif args.mode == "collect":
        if args.pg_bin is None:
            parser.error("collect requires --pg-bin")
        collect(perf, args.pg_bin, args.output, args.soak_seconds, args.startup_timeout)
    else:
        analyze(perf, args.output)


if __name__ == "__main__":
    main()
