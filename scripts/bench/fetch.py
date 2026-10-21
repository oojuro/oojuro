#!/usr/bin/env python3
"""Fetch pinned benchmark datasets into the shared, git-excluded cache.

Usage: python3 -I scripts/bench/fetch.py [--root DIR] [--pin] [--accept-nc] NAME...|all

Every file is checked against the sha256 in bench/datasets.json. A file with no
pinned sha256 is accepted only with --pin, which records the digest (and, for
BEIR zips, first checks the md5 published by BEIR). Non-commercial sets are
skipped unless --accept-nc is given. Downloads are atomic (.part then rename),
so concurrent sessions never see a half-written file; a lock serializes fetches.
"""

import argparse
import fcntl
import hashlib
import json
import os
import sys
import urllib.request
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
MANIFEST = REPO / "bench" / "datasets.json"


def digest(path, algorithm):
    h = hashlib.new(algorithm)
    with open(path, "rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def download(url, target, size):
    part = target.with_suffix(target.suffix + ".part")
    request = urllib.request.Request(url, headers={"User-Agent": "oojuro-bench-fetch/1"})
    with urllib.request.urlopen(request, timeout=120) as response, open(part, "wb") as out:
        total = 0
        while block := response.read(1 << 20):
            out.write(block)
            total += len(block)
    if size is not None and total != size:
        part.unlink()
        raise SystemExit(f"{url}: got {total} bytes, manifest says {size}")
    os.replace(part, target)


def fetch(name, spec, root, pin, accept_nc):
    if spec.get("opt_in") and not accept_nc:
        print(f"skip {name}: {spec['license']} is opt-in; pass --accept-nc")
        return False
    directory = root / name
    directory.mkdir(parents=True, exist_ok=True)
    changed = False
    for entry in spec["files"]:
        target = directory / entry["path"]
        if not target.exists():
            print(f"fetch {name}/{entry['path']} ...", flush=True)
            try:
                download(entry["url"], target, entry.get("size"))
            except Exception as error:  # optional shards may be skipped
                if entry.get("optional"):
                    print(f"  optional file failed: {error}")
                    continue
                raise
        got = digest(target, "sha256")
        if "sha256" in entry:
            if got != entry["sha256"]:
                raise SystemExit(f"{target}: sha256 {got} != pinned {entry['sha256']}")
        elif pin:
            if "md5_published" in entry and digest(target, "md5") != entry["md5_published"]:
                raise SystemExit(f"{target}: md5 differs from the published BEIR md5")
            entry["sha256"] = got
            entry["size"] = target.stat().st_size
            changed = True
            print(f"  pinned sha256 {got}")
        else:
            raise SystemExit(f"{target}: no pinned sha256; rerun with --pin and commit the manifest")
        if entry.get("extract") == "zip":
            done = directory / (entry["path"] + ".extracted")
            if not done.exists():
                with zipfile.ZipFile(target) as archive:
                    for member in archive.namelist():
                        resolved = (directory / member).resolve()
                        if not str(resolved).startswith(str(directory.resolve()) + os.sep):
                            raise SystemExit(f"unsafe zip member {member}")
                    archive.extractall(directory)
                done.write_text(got + "\n")
    (directory / "LICENSE.note").write_text(
        f"{name}: {spec['license']} ({spec['license_class']})\nsource: {spec.get('source', spec['files'][0]['url'])}\n")
    print(f"ok {name} -> {directory}")
    return changed


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("names", nargs="+")
    parser.add_argument("--root", default=os.environ.get("OOJURO_DATA", "~/.cache/oojuro-data"))
    parser.add_argument("--pin", action="store_true")
    parser.add_argument("--accept-nc", action="store_true")
    args = parser.parse_args()
    root = Path(args.root).expanduser()
    root.mkdir(parents=True, exist_ok=True)
    manifest = json.loads(MANIFEST.read_text())
    names = list(manifest["datasets"]) if args.names == ["all"] else args.names
    unknown = [n for n in names if n not in manifest["datasets"]]
    if unknown:
        raise SystemExit(f"unknown datasets {unknown}; known: {list(manifest['datasets'])}")
    with open(root / ".fetch.lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        changed = False
        for name in names:
            changed |= fetch(name, manifest["datasets"][name], root, args.pin, args.accept_nc)
    if changed:
        MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n")
        print(f"updated pins in {MANIFEST}; commit it")


if __name__ == "__main__":
    sys.exit(main())
