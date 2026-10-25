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
    parser.add_argument("--case", choices=["all", "file", "namespace", "group"], default="all")
    args = parser.parse_args()
    if platform.system() != "Linux":
        parser.exit(2, "this syscall-fault gate requires Linux\n")
    with tempfile.TemporaryDirectory(prefix="ojo-fsync-retry-") as scratch:
        work = Path(scratch)
        preload = work / "fault.so"
        compiler = shlex.split(os.environ.get("CC", "cc"))
        subprocess.run([*compiler, "-shared", "-fPIC", "-O2", "-Wall", "-Wextra", "-Werror",
                        str(ROOT / "scripts/fixtures/ojo-fsync-fault.c"), "-o", str(preload)], check=True)
        scopes = ["file", "namespace", "group-file", "group-namespace"] if args.case == "all" else (
            ["group-file", "group-namespace"] if args.case == "group" else [args.case])
        cases = [(scope, encrypted, alias) for scope in scopes for encrypted in [False, True]
                 for alias in ([False, True] if scope == "namespace" else [False])]
        for scope, encrypted, alias in cases:
            case = work / (scope + ("-alias" if alias else "") + ("-encrypted" if encrypted else "-raw"))
            case.mkdir(mode=0o700)
            env = os.environ | {"LD_PRELOAD": str(preload),
                                "OJO_SYNC_FAULT_TARGET": str(case / ("database.juro" if scope == "namespace" else "database.ojo")),
                                "OJO_SYNC_FAULT_FLAG": str(case / "fail"),
                                "OJO_SYNC_FAULT_COUNTER": str(case / "calls")}
            env.pop("OJO_SYNC_FAULT_ENCRYPTED", None)
            env.pop("OJO_SYNC_FAULT_DIRECTORY", None)
            env.pop("OJO_SYNC_FAULT_SKIP", None)
            env.pop("OJO_SYNC_FAULT_ALIAS", None)
            if encrypted:
                env["OJO_SYNC_FAULT_ENCRYPTED"] = "1"
            if scope.endswith("namespace"):
                env["OJO_SYNC_FAULT_DIRECTORY"] = str(case)
                if scope == "namespace":
                    env["OJO_SYNC_FAULT_SKIP"] = "1"
            if alias:
                env["OJO_SYNC_FAULT_ALIAS"] = "1"
            test = "group_retry" if scope.startswith("group-") else ("namespace_retry" if scope == "namespace" else "sync_retry")
            command = ["cargo", "test", "--locked", "-p", "ojo", "--test", test]
            if args.features:
                command += ["--features", args.features]
            command += ["--", "--ignored", "--exact",
                        "event_group_never_acknowledges_an_unconfirmed_prefix" if scope.startswith("group-") else (
                            "uncertain_creation_needs_directory_confirmation_before_a_new_ack" if scope == "namespace" else "complete_but_unsynced_retry_never_receives_a_durable_ack"), "--test-threads=1"]
            subprocess.run(command, cwd=ROOT, env=env, check=True)
            print(json.dumps({"case": case.name, "real_fsync_fault_gate": "passed",
                              "observed_syncs": (case / "calls").read_text(),
                              "fixture_sha256": hashlib.sha256(Path(env["OJO_SYNC_FAULT_TARGET"]).read_bytes()).hexdigest()}), flush=True)


if __name__ == "__main__":
    main()
