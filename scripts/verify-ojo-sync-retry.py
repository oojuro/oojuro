#!/usr/bin/env python3
"""Opt-in Linux real-fsync-failure gate; C compiler needed only for this gate."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--features", default="")
    args = parser.parse_args()
    if platform.system() != "Linux":
        parser.exit(2, "this syscall-fault gate requires Linux\n")
    with tempfile.TemporaryDirectory(prefix="ojo-fsync-retry-") as scratch:
        work = Path(scratch)
        preload = work / "fault.so"
        compiler = shlex.split(os.environ.get("CC", "cc"))
        subprocess.run([*compiler, "-shared", "-fPIC", "-O2", "-Wall", "-Wextra", "-Werror",
                        str(ROOT / "scripts/fixtures/ojo-fsync-fault.c"), "-o", str(preload)], check=True)
        for encrypted in [False, True]:
            case = work / ("encrypted" if encrypted else "raw")
            case.mkdir(mode=0o700)
            env = os.environ | {"LD_PRELOAD": str(preload),
                                "OJO_SYNC_FAULT_TARGET": str(case / "database.ojo"),
                                "OJO_SYNC_FAULT_FLAG": str(case / "fail"),
                                "OJO_SYNC_FAULT_COUNTER": str(case / "calls")}
            env.pop("OJO_SYNC_FAULT_ENCRYPTED", None)
            if encrypted:
                env["OJO_SYNC_FAULT_ENCRYPTED"] = "1"
            command = ["cargo", "test", "--locked", "-p", "ojo", "--test", "sync_retry"]
            if args.features:
                command += ["--features", args.features]
            command += ["--", "--ignored", "--exact",
                        "complete_but_unsynced_retry_never_receives_a_durable_ack", "--test-threads=1"]
            subprocess.run(command, cwd=ROOT, env=env, check=True)
            print(json.dumps({"case": case.name, "real_fsync_fault_gate": "passed",
                              "observed_syncs": (case / "calls").read_text(),
                              "fixture_sha256": hashlib.sha256((case / "database.ojo").read_bytes()).hexdigest()}), flush=True)


if __name__ == "__main__":
    main()
