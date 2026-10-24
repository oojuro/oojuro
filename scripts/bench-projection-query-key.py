#!/usr/bin/env python3
"""Compare frozen synthetic public projection API profiles; no provider or dataset fetch."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

ORDER = ["baseline", "candidate", "candidate", "baseline"]
SIZES = [1000, 10000, 100000]


def checked_fields(record):
    return {
        "queries": [(q["case"], q["hits"], q["signature"]) for q in record["queries"]],
        "refreshed": record["refreshed_signatures"],
        "text_bytes": record["text_bytes"],
        "heap": record["estimated_retained_heap_bytes"],
        "heap_after": record["estimated_heap_after_queries"],
        "refreshed_heap": record["refreshed_estimated_heap_bytes"],
        "refreshed_rows": record["refreshed_rows"],
    }


def verify(report):
    assert report["binaries"]["baseline"] != report["binaries"]["candidate"]
    for size in sorted({record["rows"] for record in report["runs"]}):
        assert size in SIZES
        records = [record for record in report["runs"] if record["rows"] == size]
        assert [record["binary"] for record in records] == ORDER
        expected = checked_fields(records[0])
        for record in records:
            assert record["binary_sha256"] == report["binaries"][record["binary"]]
            assert checked_fields(record) == expected, "ID/score/excerpt/heap/refresh mismatch"
            assert record["estimated_heap_after_queries"] == record["estimated_retained_heap_bytes"]
    assert report["runs"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--candidate", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--sizes", default="1000,10000,100000")
    parser.add_argument("--verify-report", type=Path, help="verify an existing combined report offline")
    args = parser.parse_args()
    if args.verify_report:
        verify(json.loads(args.verify_report.read_text()))
        print("projection result signatures and retained-heap parity verified")
        return
    if not all([args.baseline, args.candidate, args.output]):
        parser.error("explicit frozen baseline/candidate binaries and output required")
    sizes = [int(size) for size in args.sizes.split(",")]
    if not sizes or len(set(sizes)) != len(sizes) or any(size not in SIZES for size in sizes):
        parser.error("only distinct 1000,10000,100000 fixture sizes are supported")
    binaries = {name: path.resolve(strict=True) for name, path in
                [("baseline", args.baseline), ("candidate", args.candidate)]}
    hashes = {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in binaries.items()}
    assert hashes["baseline"] != hashes["candidate"], "distinct source artifacts required"
    report = {"binaries": hashes, "runs": []}
    for size in sizes:
        for order, name in enumerate(ORDER):
            label = f"{name}-{size}-{order}"
            result = subprocess.run([str(binaries[name]), str(size), label],
                                    capture_output=True, text=True, timeout=240, check=True)
            record = json.loads(result.stdout)
            assert record["rows"] == size
            record.update(binary=name, binary_sha256=hashes[name], order=order)
            report["runs"].append(record)
            print("completed", size, order, name, flush=True)
    verify(report)
    args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
