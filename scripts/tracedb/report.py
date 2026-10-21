"""Aggregate TraceBench result lines into the verdict matrix (median, min-max over runs).

Usage: python3 report.py <results.jsonl> [more.jsonl ...]  > table.md
Each cell: median [min–max] over runs; "·" = not measured.
"""
import json, statistics, sys
from collections import defaultdict


def load(paths):
    rows = []
    for p in paths:
        for line in open(p):
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def dig(d, path):
    for k in path.split("."):
        if not isinstance(d, dict) or k not in d:
            return None
        d = d[k]
    return d


def cell(vals, fmt="{:.2f}"):
    vals = [v for v in vals if v is not None]
    if not vals:
        return "·"
    m = statistics.median(vals)
    if len(vals) == 1:
        return fmt.format(m) + " (1 run)"
    return f"{fmt.format(m)} [{fmt.format(min(vals))}–{fmt.format(max(vals))}]"


OPS = [
    # (label, suite, path, format)
    ("bulk load, k events/s", "reads", "load.events_per_s", lambda v: v / 1e3),
    ("bytes / raw after load", "reads", "bytes_per_raw", None),
    ("RSS after load, MiB", "reads", "mem_after_load", lambda v: v / 2**20),
    ("peak RSS during load, MiB", "reads", "mem_peak_load", lambda v: v / 2**20),
    ("get p50 / p99 ms", "reads", "reads.get", "lat"),
    ("get_range 512 B p50 / p99 ms", "reads", "reads.get_range_512", "lat"),
    ("session p50 / p99 ms", "reads", "reads.session", "lat"),
    ("agg_session p50 / p99 ms", "reads", "reads.agg_session", "lat"),
    ("time_range 1 h (≤1000) p50 / p99 ms", "reads", "reads.time_range_1h_1000", "lat"),
    ("filter: tool calls in a day p50 / p99 ms", "reads", "reads.filter0_100", "lat"),
    ("filter: failed calls of a tool p50 / p99 ms", "reads", "reads.filter1_100", "lat"),
    ("filter: agent's results in a day p50 / p99 ms", "reads", "reads.filter2_100", "lat"),
    ("count: tool calls in a day p50 / p99 ms", "reads", "reads.count0", "lat"),
    ("pending calls, all time p50 ms", "reads", "reads.filter3_100", "lat"),
    ("agg_tools 1 day p50 / p99 ms", "reads", "reads.agg_tools_1d", "lat"),
    ("agg_tools 30 days p50 ms", "reads", "reads.agg_tools_30d", "lat"),
    ("top_sessions 1 day p50 / p99 ms", "reads", "reads.top_sessions_1d_10", "lat"),
    ("mixed 8 threads ops/s", "reads", "reads.mixed_8threads.ops_per_s", None),
    ("export MB/s", "reads", "reads.export.mb_per_s", None),
    ("delete_session p50 ms", "reads", "purge.delete_session", "lat"),
    ("purge 7 days, s", "reads", "purge.purge_before_s", None),
    ("reclaim, s", "reads", "purge.reclaim_s", None),
    ("disk after reclaim / before", "reads", None, "reclaim_ratio"),
    ("deleted needles in files before / after reclaim", "reads", None, "needles"),
    ("turn: search top-20 p50 / p99 ms", "turnloop", "turnloop.search_top20", "lat"),
    ("turn: fetch 20 excerpts p50 / p99 ms", "turnloop", "turnloop.fetch_20_excerpts", "lat"),
    ("turn: durable append p50 / p99 ms", "turnloop", "turnloop.append", "lat"),
    ("turn: whole turn p50 / p99 ms", "turnloop", "turnloop.turn", "lat"),
    ("turns/s (16 agents)", "turnloop", "turnloop.turn.ops_per_s", None),
    ("freshness failures", "turnloop", "turnloop.freshness_failures", None),
    ("ingest: durable append p50 / p99 ms", "ingest", "ingest", "lat"),
    ("ingest: events/s (16 writers)", "ingest", "ingest.items_per_s", None),
    ("ingest: bytes / raw", "ingest", "bytes_per_raw", None),
    ("ingest: peak RSS MiB", "ingest", "mem_peak", lambda v: v / 2**20),
]


def main():
    rows = load(sys.argv[1:])
    by = defaultdict(list)
    arms = []
    for r in rows:
        if r.get("suite") == "verify":
            continue
        if r["arm"].startswith("oojuro"):
            # engine version from the frozen binary name (tracebench-<commit>-<hash>)
            commit = (r.get("bin") or "").split("-")[1:2]
            r["arm"] = r["arm"] + (f" @{commit[0]}" if commit else " @v1")
        key = (r["arm"], r["suite"], r.get("tier", 1))
        by[key].append(r)
        if r["arm"] not in arms:
            arms.append(r["arm"])
    tiers = sorted({k[2] for k in by})
    for tier in tiers:
        print(f"\n### Tier {tier}\n")
        print("| operation | " + " | ".join(arms) + " |")
        print("|---|" + "---|" * len(arms))
        for label, suite, path, fmt in OPS:
            cells = []
            for arm in arms:
                rs = by.get((arm, suite, tier), [])
                if fmt == "lat":
                    p50 = [dig(r, path + ".p50_ms") for r in rs]
                    p99 = [dig(r, path + ".p99_ms") for r in rs]
                    a, b = cell(p50, "{:.3g}"), cell(p99, "{:.3g}")
                    cells.append("·" if a == "·" else f"{a} / {b}")
                elif fmt == "reclaim_ratio":
                    v = [dig(r, "purge.disk_after_reclaim") / dig(r, "purge.disk_before")
                         for r in rs if dig(r, "purge.disk_before")]
                    cells.append(cell(v, "{:.2f}"))
                elif fmt == "needles":
                    a = [dig(r, "purge.needle_files_hits_before_reclaim") for r in rs]
                    b = [dig(r, "purge.needle_files_hits_after_reclaim") for r in rs]
                    n = [dig(r, "purge.needles") for r in rs]
                    cells.append("·" if not rs else f"{cell(a, '{:.0f}')} / {cell(b, '{:.0f}')} of {cell(n, '{:.0f}')}")
                else:
                    vals = [dig(r, path) for r in rs]
                    if callable(fmt):
                        vals = [fmt(v) if v is not None else None for v in vals]
                    cells.append(cell(vals, "{:.3g}"))
            print(f"| {label} | " + " | ".join(cells) + " |")
        loads = [r["host_before"]["loadavg"].split()[0] for k, rs in by.items() if k[2] == tier for r in rs]
        print(f"\nRuns: {sum(len(v) for k, v in by.items() if k[2] == tier)}; load average at start: "
              f"{min(map(float, loads)):.0f}–{max(map(float, loads)):.0f}.")


if __name__ == "__main__":
    main()
