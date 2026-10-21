"""Aggregate .scratch/out/*.jsonl into markdown tables and committed JSON.

Usage: python3 scripts/compress/report.py [out_dir=.scratch/out] [dest=docs/benchmarks/compress]
Writes <dest>/results.json (aggregates only: ratios, medians, spreads; no text)
and prints markdown sections to stdout for docs/TRACK_COMPRESS.md.
"""
import json, os, statistics, sys

OUT = sys.argv[1] if len(sys.argv) > 1 else ".scratch/out"
DEST = sys.argv[2] if len(sys.argv) > 2 else "docs/benchmarks/compress"
CORPORA = ["swe", "ohjson", "othink", "hermes", "ohsft", "fiqa"]


def rows(name):
    p = os.path.join(OUT, name)
    if not os.path.exists(p):
        return []
    out = []
    for line in open(p):
        line = line.strip()
        if line.startswith("{"):
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                pass
    return out


def med(v):
    return statistics.median(v) if v else float("nan")


def spread(v):
    return (min(v), max(v)) if v else (float("nan"), float("nan"))


def f4(x):
    return "—" if x is None or x != x else f"{x:.4f}"


def f1(x):
    return "—" if x is None or x != x else f"{x:,.1f}"


results = {}

floors = {}
for name in ("floors.swe.jsonl", "floors.ohjson.jsonl", "floors.rest.jsonl"):
    for d in rows(name):
        floors[d["corpus"]] = {"raw": d["raw"], "zstd19_long27": d["zstd19_long27"] / d["raw"], "xz9": d["xz9"] / d["raw"]}
results["floors"] = floors

sc = {}
for d in rows("scorecard.jsonl") + rows("scorecard-archive.jsonl") + rows("scorecard-p32.jsonl"):
    s = sc.setdefault((d.get("label", "default"), d["corpus"]), {"build": [], "read": []})
    s[d["phase"]].append(d)
results["scorecard"] = {}
for (label, c), s in sorted(sc.items()):
    b, r = s["build"], s["read"]
    if not b:
        continue
    comp = [x["compaction"]["mbps"] for x in b if x.get("compaction")]
    agg = {
        "reps": len(b),
        "raw": b[0]["raw"],
        "hot_ratio": med([x["hot_ratio"] for x in b]),
        "ratio": med([x["ratio"] for x in b]),
        "ratio_spread": spread([x["ratio"] for x in b]),
        "append_us_p50": med([x["append_us_p50"] for x in b]),
        "append_us_p50_spread": spread([x["append_us_p50"] for x in b]),
        "append_us_p99": med([x["append_us_p99"] for x in b]),
        "append_us_p99_spread": spread([x["append_us_p99"] for x in b]),
        "compact_mbps": med(comp),
        "compact_mbps_spread": spread(comp),
        "verify_mismatches": sum(x["verify_mismatches"] for x in b),
        "append_during_us_p50": med([x["append_during_compaction_us_p50"] for x in b if x.get("append_during_compaction_us_p50") == x.get("append_during_compaction_us_p50") and x.get("append_during_compaction_us_p50") is not None]),
        "append_during_us_p99": med([x["append_during_compaction_us_p99"] for x in b if x.get("append_during_compaction_us_p99") is not None]),
    }
    if r:
        agg.update({
            "open_ms": med([x["open_ms"] for x in r]),
            "open_ms_spread": spread([x["open_ms"] for x in r]),
            "rss_mib": med([x["rss_kib_after"] / 1024 for x in r]),
            "rss_ratio": med([x["rss_ratio"] for x in r]),
            "q20_us_p50": med([x["query20_us_p50"] for x in r]),
            "q20_us_p50_spread": spread([x["query20_us_p50"] for x in r]),
            "q20_us_p99": med([x["query20_us_p99"] for x in r]),
            "q20_us_p99_spread": spread([x["query20_us_p99"] for x in r]),
        })
    results["scorecard"][f"{label}/{c}"] = agg

results["baselines"] = {}
for d in rows("baselines.jsonl"):
    key = f"{d['system']}/{d['corpus']}" + ("/read" if d.get("phase") == "read" else "")
    results["baselines"][key] = {k: d[k] for k in ("raw", "disk", "ratio", "rss_ratio", "open_ms", "records") if k in d}

wl = {}
for d in rows("whole.jsonl"):
    wl.setdefault((d["codec"], d["corpus"]), []).append(d)
results["whole"] = {}
for (codec, c), v in wl.items():
    results["whole"][f"{codec}/{c}"] = {
        "ratio": v[0]["ratio"],
        "c_mbps": med([x["c_mbps"] for x in v]),
        "d_mbps": med([x["d_mbps"] for x in v]),
        "c_mbps_spread": spread([x["c_mbps"] for x in v]),
        "d_mbps_spread": spread([x["d_mbps"] for x in v]),
        "runs": len(v),
    }

results["sweep"] = [
    {k: d[k] for k in ("corpus", "frame_bytes", "avg_chunk", "level", "dict_bytes", "ratio", "ref_bytes_fraction", "prefix", "pframes", "frames") if k in d}
    for d in rows("sweep.jsonl")
]

os.makedirs(DEST, exist_ok=True)
json.dump(results, open(os.path.join(DEST, "results.json"), "w"), indent=1, sort_keys=True)

for label, title in [("default", "default profile (64 KiB frames)"), ("archive", "archive profile (256 KiB frames, 1 MiB dict)"), ("p32", "default profile on the 32 MiB prefixes the journal was measured on")]:
    keys = [c for c in CORPORA if f"{label}/{c}" in results["scorecard"]]
    if not keys:
        continue
    print(f"### Store scorecard: {title}, median (min–max) of reps\n")
    print("| corpus | raw MB | compacted | zstd -19 --long=27 | xz -9 | append p50 / p99 µs (quiet) | append p50 / p99 µs (during compaction) | top-20 excerpt p50 / p99 µs | RSS MiB (RSS/raw) | open ms | compaction MB/s (4 threads) | reps |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for c in keys:
        a = results["scorecard"][f"{label}/{c}"]
        fl = floors.get(c, {}) if label != "p32" else {}
        rs = a["ratio_spread"]
        print(f"| {c} | {a['raw']/1e6:,.0f} | **{f4(a['ratio'])}** ({f4(rs[0])}–{f4(rs[1])}) | {f4(fl.get('zstd19_long27'))} | {f4(fl.get('xz9'))} | "
              f"{a['append_us_p50']:,.0f} / {a['append_us_p99']:,.0f} | {a.get('append_during_us_p50', float('nan')):,.0f} / {a.get('append_during_us_p99', float('nan')):,.0f} | "
              f"{a.get('q20_us_p50', float('nan')):,.0f} / {a.get('q20_us_p99', float('nan')):,.0f} | "
              f"{a.get('rss_mib', float('nan')):,.1f} ({f4(a.get('rss_ratio'))}) | {a.get('open_ms', float('nan')):,.1f} | {f1(a['compact_mbps'])} | {a['reps']} |")
    print()

print("### Whole-sample leaderboard (64 MiB sample, bytes/raw; MB/s single-thread)\n")
codecs = sorted({k.rsplit('/', 1)[0] for k in results["whole"]}, key=lambda s: (s.split(':')[0], s))
print("| codec | " + " | ".join(CORPORA) + " | c MB/s (swe) | d MB/s (swe) |")
print("|---|" + "---|" * len(CORPORA) + "---|---|")
for k in codecs:
    cells = [f4(results["whole"][f"{k}/{c}"]["ratio"]) if f"{k}/{c}" in results["whole"] else "…" for c in CORPORA]
    s = results["whole"].get(f"{k}/swe")
    flag = "" if s and s["runs"] >= 3 else " †"
    print(f"| {k} | " + " | ".join(cells) + f" | {f1(s['c_mbps']) + flag if s else '…'} | {f1(s['d_mbps']) + flag if s else '…'} |")
print("\n† single run (speed-panel codecs are medians of 3).\n")
