#!/usr/bin/env python3
"""How much a bounded search agrees with the exact one.

Compares two `searchprobe run --dump` files line by line (same queries, same
order) and prints, as JSON: the share of queries whose ranked list is
identical, the mean overlap of the two top-k id sets, the share with the same
top-1, and the mean rank-biased overlap (p = 0.9) of the two lists.

    python3 -I scripts/perfsearch/overlap.py exact.txt bounded.txt [--k 20]
"""
import json
import sys


def ids(line):
    plan, agent, i, bits, entropy = line.rstrip("\n").split(" ")
    return i.split(",") if i else []


def rbo(a, b, p=0.9):
    depth = max(len(a), len(b))
    if depth == 0:
        return 1.0
    seen_a, seen_b, score = set(), set(), 0.0
    for d in range(1, depth + 1):
        if d <= len(a):
            seen_a.add(a[d - 1])
        if d <= len(b):
            seen_b.add(b[d - 1])
        score += (p ** (d - 1)) * len(seen_a & seen_b) / d
    return (1 - p) * score


def main(argv):
    k = int(argv[argv.index("--k") + 1]) if "--k" in argv else 20
    with open(argv[1]) as f:
        x = f.readlines()
    with open(argv[2]) as f:
        y = f.readlines()
    if len(x) != len(y):
        print(json.dumps({"error": f"{len(x)} vs {len(y)} lines"}))
        return 1
    same = top1 = 0
    ov = rb = 0.0
    for p, q in zip(x, y):
        a, b = ids(p)[:k], ids(q)[:k]
        same += a == b
        top1 += a[:1] == b[:1]
        ov += len(set(a) & set(b)) / max(len(a), 1)
        rb += rbo(a, b)
    n = max(len(x), 1)
    print(json.dumps({"queries": len(x), "identical_lists": same / n, "same_top1": top1 / n,
                      "mean_overlap_at_k": ov / n, "mean_rbo_0.9": rb / n, "k": k}))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
