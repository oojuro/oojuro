"""Leaderboard v2 tables (docs/TRACK_COMPRESS.md) from .scratch/out.

Usage: python3 -I scripts/compress/report2.py [out_dir=.scratch/out] [dest=docs/benchmarks/compress/leaderboard-v2.json]

Reads whole2.jsonl, blocks2.jsonl, run2.jsonl, cold-ext.jsonl, think2.jsonl
(scripts/compress/leaderboard2.sh, cold_tier.sh) and ext-codecs.jsonl
(external/run_external.sh aggregate). Sizes come from the unlocked "ratio"
pass on the full 64 MiB samples; speeds and excerpt latencies from the
locked "time" pass (whole: swe from the track's locked run, othink, devlogs;
blocks: 16 MiB prefixes of swe, othink, devlogs; run profiles: 64 MiB of swe,
othink, devlogs), reported as the median over those corpora. Writes aggregates
only (no text) to the JSON file and prints markdown to stdout.
"""
import json, os, statistics, sys

OUT = sys.argv[1] if len(sys.argv) > 1 else ".scratch/out"
DEST = sys.argv[2] if len(sys.argv) > 2 else "docs/benchmarks/compress/leaderboard-v2.json"
CORPORA = ["swe", "ohjson", "othink", "hermes", "ohsft", "fiqa", "devlogs"]
S3 = 0.023  # USD per GB-month (S3 Standard list price)


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
    v = [x for x in v if x is not None and x == x]
    return statistics.median(v) if v else None


def f4(x):
    return "…" if x is None else f"{x:.4f}"


def fs(x):
    if x is None:
        return "…"
    if x >= 100:
        return f"{x:,.0f}"
    if x >= 10:
        return f"{x:.1f}"
    if x >= 1:
        return f"{x:.2f}"
    return f"{x:.3f}"


def bold_min(cells):
    vals = [c for c in cells if c is not None]
    best = min(vals) if vals else None
    return [("**" + f4(c) + "**") if c is not None and c == best else f4(c) for c in cells]


res = {"whole": {}, "blocks": {}, "run": {}, "cold": {}, "think": {}, "external": {}}

# ---------------------------------------------------------------- whole
W = rows("whole2.jsonl")
wr, wt = {}, {}
for d in W:
    key = d["codec"]
    if d["pass"] == "ratio":
        wr.setdefault(key, {})[d["corpus"]] = d["ratio"]
    else:
        wt.setdefault(key, []).append(d)
ext = rows("ext-codecs.jsonl")
EXT_ORDER = [("zpaq", "-method 5"), ("zpaq", "-method 3"), ("zpaq", "-method 1"), ("bsc", "-b64e2"), ("bsc", "-b64"),
             ("bsc", "-b16"), ("openzl", "-l 7"), ("openzl", "-l 6"), ("paq8px", "-8"), ("repair-wan", "-i"), ("repair-nav", "")]
EXT_LABEL = {"zpaq": "zpaq 7.15", "bsc": "libbsc 3.3.12", "openzl": "OpenZL 0.3 serial", "paq8px": "paq8px v217",
             "repair-wan": "Re-Pair (Wan)", "repair-nav": "Re-Pair (Navarro, no entropy coder)"}
for d in ext:
    if d.get("ratio") is None:
        continue
    k = f"{EXT_LABEL.get(d['codec'], d['codec'])} {d['args']}".strip()
    e = res["external"].setdefault(k, {"ratio": {}, "prefix_bytes": d["prefix_bytes"], "c_mbps": [], "d_mbps": [], "timing": d.get("timing")})
    e["ratio"][d["corpus"]] = d["ratio"]
    if d.get("c_mbps_median"):
        e["c_mbps"].append(d["c_mbps_median"])
        e["d_mbps"].append(d["d_mbps_median"])

print("### Whole-sample leaderboard (64 MiB samples; bytes / raw byte; best per corpus in bold)\n")
print("Speeds: single-thread MB/s, median over the locked runs (swe and ohjson from the track's locked pass, othink and devlogs from this one); external codecs as measured by `run_external.sh` (unlocked, †).\n")
print("| codec | " + " | ".join(CORPORA) + " | compress MB/s | decompress MB/s |")
print("|---|" + "---|" * (len(CORPORA) + 2))
order = sorted(wr, key=lambda k: statistics.mean([wr[k][c] for c in CORPORA if c in wr[k]] or [9]))
allbest = {c: min([wr[k][c] for k in wr if c in wr[k]] + [e["ratio"][c] for e in res["external"].values() if c in e["ratio"] and e["prefix_bytes"] == 0] or [None]) for c in CORPORA}
for k in order:
    t = wt.get(k, [])
    cs, ds = med([x["c_mbps"] for x in t]), med([x["d_mbps"] for x in t])
    cells = [wr[k].get(c) for c in CORPORA]
    txt = [("**" + f4(v) + "**") if v is not None and v == allbest[c] else f4(v) for v, c in zip(cells, CORPORA)]
    print(f"| {k} | " + " | ".join(txt) + f" | {fs(cs)} | {fs(ds)} |")
    res["whole"][k] = {"ratio": wr[k], "c_mbps": cs, "d_mbps": ds, "timed_corpora": sorted({x["corpus"] for x in t})}
for k, _ in EXT_ORDER:
    pass
for k in sorted(res["external"], key=lambda k: statistics.mean(list(res["external"][k]["ratio"].values()))):
    e = res["external"][k]
    pre = e["prefix_bytes"]
    cells = [e["ratio"].get(c) for c in CORPORA]
    txt = [("**" + f4(v) + "**") if v is not None and pre == 0 and v == allbest[c] else f4(v) for v, c in zip(cells, CORPORA)]
    note = " (first 8 MiB)" if pre else ""
    mark = "" if e["timing"] == "locked" else " †"
    print(f"| {k}{note} | " + " | ".join(txt) + f" | {fs(med(e['c_mbps']))}{mark} | {fs(med(e['d_mbps']))}{mark} |")
print("| cmix v21 | skipped (2026-10-21): needs ~32 GB RAM and runs at ~2 KB/s; paq8px gives the floor | | | | | | | | |")
print("\n† single unlocked run, indicative.\n")

# ---------------------------------------------------------------- blocks
B = rows("blocks2.jsonl")
R = rows("run2.jsonl")
br, bt = {}, {}
for d in B:
    key = (d["block_bytes"], d["codec"] + (" +dict" if d.get("dict_req", 0) > 0 and d["codec"] != "fsst" else ""))
    if d["pass"] == "ratio":
        br.setdefault(key, {})[d["corpus"]] = d["ratio"]
    else:
        bt.setdefault(key, []).append(d)
rr, rt = {}, {}
for d in R:
    (rr if d["pass"] == "ratio" else rt).setdefault(d["codec"], {} if d["pass"] == "ratio" else [])
    if d["pass"] == "ratio":
        rr[d["codec"]][d["corpus"]] = d["ratio"]
    else:
        rt[d["codec"]].append(d)

SIZES = [(0, "one record per block"), (4096, "4 KiB"), (16384, "16 KiB"), (65536, "64 KiB"), (262144, "256 KiB")]
RUN_AT = {65536: ["oojuro-run:default", "oojuro-run:flush"], 262144: ["oojuro-run:archive"]}
headline = []
for b, label in SIZES:
    keys = [k for k in br if k[0] == b]
    if not keys:
        continue
    title = "one record per block" if b == 0 else f"{label} blocks"
    print(f"### Random access, {title} (bytes / raw byte incl. dictionary and 16 B per block; 1 KiB excerpt fetch)\n")
    print("| codec | " + " | ".join(CORPORA) + " | compress MB/s | decompress MB/s | fetch p50 µs | fetch p99 µs |")
    print("|---|" + "---|" * (len(CORPORA) + 4))
    entries = []
    for k in keys:
        t = bt.get(k, [])
        entries.append((k[1], br[k], t))
    for name in RUN_AT.get(b, []):
        if name in rr:
            entries.append((name.replace("oojuro-run:", "**oojuro** run, ") + " profile", rr[name], rt.get(name, [])))
    best = {c: min([e[1][c] for e in entries if c in e[1]] or [None]) for c in CORPORA}
    entries.sort(key=lambda e: statistics.mean([e[1][c] for c in CORPORA if c in e[1]] or [9]))
    for name, r, t in entries:
        cells = [r.get(c) for c in CORPORA]
        txt = [("**" + f4(v) + "**") if v is not None and v == best[c] else f4(v) for v, c in zip(cells, CORPORA)]
        agg = {m: med([x.get(m) for x in t]) for m in ("c_mbps", "d_mbps", "fetch_us_p50", "fetch_us_p99")}
        print(f"| {name} | " + " | ".join(txt) + f" | {fs(agg['c_mbps'])} | {fs(agg['d_mbps'])} | {fs(agg['fetch_us_p50'])} | {fs(agg['fetch_us_p99'])} |")
        res["blocks"].setdefault(str(b), {})[name] = {"ratio": r, **agg, "timed_corpora": sorted({x["corpus"] for x in t}),
                                                      "load1": [x.get("load1") for x in t]}
    win = {c: min(((e[1][c], e[0]) for e in entries if c in e[1]), default=(None, None)) for c in CORPORA}
    headline.append((label, win))
    print()

print("### Best codec per block size and corpus (bytes / raw byte)\n")
print("| block | " + " | ".join(CORPORA) + " |")
print("|---|" + "---|" * len(CORPORA))
for label, win in headline:
    print(f"| {label} | " + " | ".join(f"{f4(v)} {n}" if v is not None else "…" for v, n in (win[c] for c in CORPORA)) + " |")
print()

# ---------------------------------------------------------------- run profiles
print("### Oojuro's own run format on the same samples (bytes / raw byte; one thread)\n")
print("| profile | " + " | ".join(CORPORA) + " | compress MB/s | scan MB/s | 1 KiB fetch p50 / p99 µs |")
print("|---|" + "---|" * (len(CORPORA) + 3))
for name in ["oojuro-run:flush", "oojuro-run:default", "oojuro-run:archive", "oojuro-run:cold4", "oojuro-run:cold16"]:
    if name not in rr:
        continue
    t = rt.get(name, [])
    agg = {m: med([x.get(m) for x in t]) for m in ("c_mbps", "d_mbps", "fetch_us_p50", "fetch_us_p99")}
    print(f"| {name.split(':')[1]} | " + " | ".join(f4(rr[name].get(c)) for c in CORPORA) +
          f" | {fs(agg['c_mbps'])} | {fs(agg['d_mbps'])} | {fs(agg['fetch_us_p50'])} / {fs(agg['fetch_us_p99'])} |")
    res["run"][name] = {"ratio": rr[name], **agg, "timed_corpora": sorted({x["corpus"] for x in t})}
print()

# ---------------------------------------------------------------- cold tier
C = rows("cold-ext.jsonl")
cold = {}
for d in C:
    name = {"bsc:e1": "libbsc -e1", "bsc:e2": "libbsc -e2", "zpaq:3": "zpaq -m3", "zpaq:5": "zpaq -m5"}[d["codec"]]
    e = cold.setdefault((name, d["block_mib"]), {"ratio": {}, "time": []})
    if d["mode"] == "size":
        e["ratio"][d["corpus"]] = d["ratio"]
        e.setdefault("cpu", []).append((d["raw"], d["c_cpu_s"], d["blocks_encoded"], d["blocks_decoded"], d["d_cpu_s"]))
    else:
        e["time"].append(d)
for (b, codec), r in br.items():
    if b >= 1 << 20:
        cold[(codec, b >> 20)] = {"ratio": r, "time": bt.get((b, codec), []), "inproc": True}
for name, mib in (("oojuro-run:cold4", 4), ("oojuro-run:cold16", 16)):
    if name in rr:
        cold[(f"**oojuro run**, {mib} MiB frames (dedupe + dict)", mib)] = {"ratio": rr[name], "time": rt.get(name, []), "inproc": True}
default = rr.get("oojuro-run:default", {})
print("### Cold archive tier candidates (independent blocks of 1-16 MiB; bytes / raw byte)\n")
print("Fetch = time to return one 1 KiB excerpt = decode its block (bsc and zpaq always decode the whole block; the in-process coders stop at the excerpt). External coders: one block encoded and decoded under the lock per (corpus, size), CPU MB/s from the size pass. Savings: S3 at $0.023 per GB-month, per TB of raw text kept, vs the default run profile on the same corpus (originals only; the index is unchanged).\n")
print("| codec | block | " + " | ".join(CORPORA) + " | compress MB/s (CPU) | decompress MB/s (CPU) | 1 KiB fetch p50 | saved $/TB-month (devlogs) | saved $/TB-month (median, agent corpora) |")
print("|---|---|" + "---|" * (len(CORPORA) + 5))
print("| **oojuro run, default** | 64 KiB | " + " | ".join(f4(default.get(c)) for c in CORPORA) + f" | {fs(res['run'].get('oojuro-run:default', {}).get('c_mbps'))} | {fs(res['run'].get('oojuro-run:default', {}).get('d_mbps'))} | {fs(res['run'].get('oojuro-run:default', {}).get('fetch_us_p50'))} µs | 0 | 0 |")
AGENT = ["swe", "ohjson", "othink", "hermes", "ohsft", "devlogs"]
for (name, mib), e in sorted(cold.items(), key=lambda kv: (kv[0][1], statistics.mean(list(kv[1]["ratio"].values()) or [9]))):
    r = e["ratio"]
    if e.get("inproc"):
        cs = med([x["c_mbps"] for x in e["time"]])
        ds = med([x["d_mbps"] for x in e["time"]])
        fetch = med([x["fetch_us_p50"] for x in e["time"]])
        fetch_s = f"{fs(fetch / 1000)} ms" if fetch else "…"
    else:
        cpu = e.get("cpu", [])
        cs = med([raw / 1e6 / c for raw, c, ne, nd, dc in cpu if c > 0])
        ds = med([(raw / ne * nd) / 1e6 / dc for raw, c, ne, nd, dc in cpu if dc > 0 and nd > 0])
        tw = [x["d_wall_s"] / max(x["blocks_decoded"], 1) for x in e["time"]]
        fetch = med(tw)
        fetch_s = f"{fs(fetch * 1000)} ms" if fetch else "…"
    sav_devlogs = (default["devlogs"] - r["devlogs"]) * 1000 * S3 if "devlogs" in r and "devlogs" in default else None
    sav_mean = med([(default[c] - r[c]) * 1000 * S3 for c in AGENT if c in r and c in default])
    print(f"| {name} | {mib} MiB | " + " | ".join(f4(r.get(c)) for c in CORPORA) +
          f" | {fs(cs)} | {fs(ds)} | {fetch_s} | {'…' if sav_devlogs is None else f'{sav_devlogs:+.2f}'} | {'…' if sav_mean is None else f'{sav_mean:+.2f}'} |")
    res["cold"][f"{name} {mib} MiB"] = {"ratio": r, "c_mbps": cs, "d_mbps": ds, "fetch_p50": fetch, "saved_usd_per_tb_month_devlogs": sav_devlogs,
                                         "saved_usd_per_tb_month_agent_median": sav_mean}
print()

# ---------------------------------------------------------------- thinking only
T = rows("think2.jsonl")
if T:
    print("### Thinking text alone (records grouped by kind, no dictionary; bytes / raw thinking byte)\n")
    print("| corpus | block | " + " | ".join(sorted({d['codec'] for d in T})) + " |")
    codecs = sorted({d["codec"] for d in T})
    print("|---|---|" + "---|" * len(codecs))
    grid = {}
    for d in T:
        kr = d.get("kind_ratio") or {}
        if "thinking" in kr:
            grid.setdefault((d["corpus"], d["block_bytes"]), {})[d["codec"]] = kr["thinking"]
    for (c, b), g in sorted(grid.items()):
        print(f"| {c} | {b >> 10} KiB | " + " | ".join(f4(g.get(k)) for k in codecs) + " |")
        res["think"][f"{c} {b}"] = g
    print()

os.makedirs(os.path.dirname(DEST), exist_ok=True)
json.dump(res, open(DEST, "w"), indent=1, sort_keys=True)
print(f"<!-- aggregates written to {DEST} -->")
