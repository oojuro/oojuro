#!/usr/bin/env python3
"""Compare two `searchprobe run --dump` files, before and after the
background floor (docs/SEARCH_FIXES.md, F3), using the after run's
`--work` file (its last column is the background pad mass; 0 = not padded).

A query whose background was not padded must be identical (ids, score bits,
entropy); only padded queries may differ. Prints counts as JSON; exits 1
when an unpadded query differs.

    python3 -I scripts/perfsearch/pad_compare.py before.txt after.txt after-work.txt
"""
import json
import sys


def main(argv):
    before, after, work = argv[1], argv[2], argv[3]
    pad = {}
    with open(work) as f:
        for line in f:
            p = line.split()
            pad[p[0]] = int(p[-1])
    with open(before) as f:
        a = f.readlines()
    with open(after) as f:
        b = f.readlines()
    out = {"lines": len(a), "unpadded": 0, "unpadded_identical": 0,
           "padded": 0, "padded_identical": 0, "padded_same_ids": 0,
           "padded_same_top1": 0, "unpadded_differs": 0}
    if len(a) != len(b):
        print(json.dumps({"error": f"{len(a)} vs {len(b)} lines"}))
        return 1
    for x, y in zip(a, b):
        plan = x.split(" ")[0]
        fx, fy = x.split(" "), y.split(" ")
        if pad.get(plan, 0) == 0:
            out["unpadded"] += 1
            if x == y:
                out["unpadded_identical"] += 1
            else:
                out["unpadded_differs"] += 1
        else:
            out["padded"] += 1
            out["padded_identical"] += x == y
            out["padded_same_ids"] += fx[2] == fy[2]
            out["padded_same_top1"] += fx[2].split(",")[0] == fy[2].split(",")[0]
    print(json.dumps(out))
    return 1 if out["unpadded_differs"] else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
