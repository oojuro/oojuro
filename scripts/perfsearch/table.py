#!/usr/bin/env python3
"""Summarize searchprobe JSON lines (scripts/perfsearch/rerun.sh) as a
markdown table: per store and query size, per arm, the median over rounds of
CPU p50/p99 and wall p50/p99 per query, with load averages.

usage: table.py .scratch/perfsearch/results/probe.jsonl
"""
import json
import statistics
import sys
from collections import defaultdict

runs = defaultdict(list)
for line in open(sys.argv[1]):
    r = json.loads(line)
    arm, store = r["label"].rsplit("-", 1)
    q = r.get("query_threads", 1)
    if q != 1:
        arm = f"{arm} q{q}"
    runs[(store, r["size"], arm)].append(r)


def med(rs, *path):
    vals = []
    for r in rs:
        v = r["rounds"][0]
        for p in path:
            v = v[p]
        vals.append(v)
    return statistics.median(vals)


print("| store | query | arm | rounds | CPU p50 / p99 ms | wall p50 / p99 ms | load |")
print("|---|---|---|---:|---:|---:|---|")
for (store, size, arm), rs in sorted(runs.items()):
    name = "turnloop 200 B" if size == 0 else (f"{size // 1024} KB" if size >= 1024 else f"{size} B")
    loads = " ".join(r["loadavg_before"].split()[0] for r in rs)
    print(
        f"| {store} | {name} | {arm} | {len(rs)} | {med(rs, 'cpu', 'p50_ms'):.2f} / {med(rs, 'cpu', 'p99_ms'):.2f} "
        f"| {med(rs, 'p50_ms'):.2f} / {med(rs, 'p99_ms'):.2f} | {loads} |"
    )
