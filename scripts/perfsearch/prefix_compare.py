#!/usr/bin/env python3
"""Compare two `searchprobe run --dump` files, before and after the anchored
continuation (docs/SEARCH_FIXES.md, F2).

Every line must be identical except where the reference returned fewer than
k results: there the new list must start with the reference list (same ids,
same score bits) and may only add results after it. Prints counts as JSON;
exits 1 on any other difference.

    python3 -I scripts/perfsearch/prefix_compare.py ref.txt final.txt [--k 20]
"""
import json
import sys


def fields(line):
    plan, agent, ids, bits, entropy = line.rstrip("\n").split(" ")
    return plan, agent, ids.split(",") if ids else [], bits.split(",") if bits else [], entropy


def main(argv):
    k = 20
    if "--k" in argv:
        k = int(argv[argv.index("--k") + 1])
    ref, new = argv[1], argv[2]
    with open(ref) as f:
        a = f.readlines()
    with open(new) as f:
        b = f.readlines()
    if len(a) != len(b):
        print(json.dumps({"error": f"{len(a)} vs {len(b)} lines"}))
        return 1
    out = {"lines": len(a), "identical": 0, "continued": 0, "other": 0,
           "short_before": 0, "short_after": 0, "added_results": 0,
           "only_entropy_differs": 0}
    shown = 0
    for x, y in zip(a, b):
        px, gx, ix, bx, ex = fields(x)
        py, gy, iy, by, ey = fields(y)
        out["short_before"] += len(ix) < k
        out["short_after"] += len(iy) < k
        if x == y:
            out["identical"] += 1
            continue
        if (px, gx, ix, bx) == (py, gy, iy, by):
            out["only_entropy_differs"] += 1
            continue
        prefix = (px, gx) == (py, gy) and len(ix) < k and len(iy) > len(ix) \
            and iy[:len(ix)] == ix and by[:len(bx)] == bx
        if prefix:
            out["continued"] += 1
            out["added_results"] += len(iy) - len(ix)
        else:
            out["other"] += 1
            if shown < 5:
                print("- " + x.rstrip("\n"), file=sys.stderr)
                print("+ " + y.rstrip("\n"), file=sys.stderr)
                shown += 1
    print(json.dumps(out))
    return 1 if out["other"] or out["only_entropy_differs"] else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
