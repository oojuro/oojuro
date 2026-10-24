#!/usr/bin/env python3
"""Sequential fixed-fixture disk profiles; preserve failures, raw tails and resources."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument("--candidate", type=Path, required=True)
parser.add_argument("--control", type=Path)
parser.add_argument("--candidate-source", required=True)
parser.add_argument("--control-source")
parser.add_argument("--out", type=Path, required=True)
parser.add_argument("--counts", default="1000,10000")
parser.add_argument("--phases", default="isolated,mixed")
parser.add_argument("--writes", default="direct,grouped")
parser.add_argument("--arms", default="candidate,sqlite,jsonl,jsonl-zstd")
parser.add_argument("--clients", type=int, default=10)
parser.add_argument("--searches", type=int, default=6)
parser.add_argument("--timeout", type=int, default=1200)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
lock_path = Path.home() / ".cache/oojuro-bench.lock"
lock_path.parent.mkdir(exist_ok=True)
lock = lock_path.open("a")
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)

binaries = {"candidate": args.candidate.resolve()}
if args.control:
    binaries["control"] = args.control.resolve()
provenance = {
    label: {
        "source": args.candidate_source if label == "candidate" else args.control_source,
        "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
    }
    for label, binary in binaries.items()
}
(args.out / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")


def system_state():
    result = {"logical_cpus": os.cpu_count(), "load_average": os.getloadavg()}
    for name, path in {
        "cpu_quota": "/sys/fs/cgroup/cpu.max",
        "memory_limit": "/sys/fs/cgroup/memory.max",
        "cpu_pressure": "/proc/pressure/cpu",
        "io_pressure": "/proc/pressure/io",
        "memory_pressure": "/proc/pressure/memory",
    }.items():
        try:
            result[name] = Path(path).read_text().strip()
        except OSError:
            pass
    return result


def stop_group(process):
    if process.poll() is not None:
        return
    os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()


summary = []
for count in map(int, args.counts.split(",")):
    for phase in args.phases.split(","):
        for write_repetition, write_mode in enumerate(args.writes.split(",")):
            for repetition, label in enumerate(args.arms.split(",")):
                if label == "control" and label not in binaries:
                    raise SystemExit("--control is required for control runs")
                binary = binaries.get(label, binaries["candidate"])
                arm = "native-disk" if label in binaries else label
                stem = f"{count}-{phase}-{write_repetition:02}-{write_mode}-{repetition:02}-{label}"
                directory = args.out / stem
                output = args.out / f"{stem}.json"
                resources = args.out / f"{stem}.resources"
                stderr_path = args.out / f"{stem}.stderr"
                command = [
                    "/usr/bin/time", "-v", "-o", str(resources), str(binary),
                    str(directory), arm, str(count), str(args.clients),
                    str(args.searches), phase, write_mode,
                ]
                entry = {"run": stem, "system_before": system_state()}
                began = time.monotonic()
                print(json.dumps({"starting": stem}), flush=True)
                with output.open("w") as stdout, stderr_path.open("w") as stderr:
                    process = subprocess.Popen(
                        command, stdout=stdout, stderr=stderr,
                        start_new_session=True,
                        env={**os.environ, "LC_ALL": "C"},
                    )
                    try:
                        entry["exit_code"] = process.wait(timeout=args.timeout)
                    except subprocess.TimeoutExpired:
                        stop_group(process)
                        entry["timeout_seconds"] = args.timeout
                    except BaseException:
                        stop_group(process)
                        raise
                entry["wall_seconds"] = time.monotonic() - began
                entry["system_after"] = system_state()
                if entry.get("exit_code") == 0:
                    entry["result"] = json.loads(output.read_text())
                    for line in resources.read_text().splitlines():
                        if "Maximum resident set size (kbytes):" in line:
                            entry["max_rss_kib"] = int(line.rsplit(":", 1)[1])
                else:
                    entry["error"] = stderr_path.read_text()[-4000:]
                summary.append(entry)
                (args.out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
                print(json.dumps({key: value for key, value in entry.items()
                                  if key not in ("result", "system_before", "system_after")}), flush=True)
                if "result" not in entry:
                    raise SystemExit("Failure retained; diagnose before increasing scale.")
print("complete", flush=True)
