"""Distributions, correlations and top windows of the four signals.

    nice -n 19 python3 -I analyze.py windows.jsonl report.json [tops.jsonl]

`report.json` holds aggregate numbers only (safe to quote). `tops.jsonl`
(optional, keep it local) lists the top 20 windows per signal by writer
number, time span and record ids, for hand-checking with `peek.py`.
Windows from the first 3 days (warm-up: estimators without history) are
left out.
"""
import json
import sys
from math import sqrt

MIN_EVENTS = {"5m": 20, "1h": 60, "5m/s": 20, "1h/s": 60, "session": 60}
SIGNALS = ["events", "calls", "zs", "zd", "cdc_new", "ng", "zs_per_ev", "density", "errors", "err_bits",
           "unusual_bits", "u_rate", "u_mix", "u_err", "u_new", "u_nov", "repeat_frac", "new_items"]


def ranks(v):
    order = sorted(range(len(v)), key=lambda i: v[i])
    r = [0.0] * len(v)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and v[order[j + 1]] == v[order[i]]:
            j += 1
        avg = (i + j) / 2 + 1
        for k in range(i, j + 1):
            r[order[k]] = avg
        i = j + 1
    return r


def spearman(a, b):
    ra, rb = ranks(a), ranks(b)
    n = len(a)
    ma, mb = sum(ra) / n, sum(rb) / n
    cov = sum((x - ma) * (y - mb) for x, y in zip(ra, rb))
    va = sqrt(sum((x - ma) ** 2 for x in ra))
    vb = sqrt(sum((y - mb) ** 2 for y in rb))
    return cov / (va * vb) if va and vb else 0.0


def quantiles(v):
    s = sorted(v)
    n = len(s)
    q = lambda p: s[min(n - 1, int(p * n))]  # noqa: E731
    return {"n": n, "p10": q(0.10), "p50": q(0.50), "p90": q(0.90), "p99": q(0.99), "max": s[-1]}


def main():
    src, dst = sys.argv[1], sys.argv[2]
    tops_path = sys.argv[3] if len(sys.argv) > 3 else None
    wins = {}
    for line in open(src):
        w = json.loads(line)
        if w["warm"]:
            continue
        w["repeat_frac"] = w["repeat"] / w["calls"] if w["calls"] else 0.0
        w["new_items"] = w["new_tool"] + w["new_files"]
        for k in ("zd", "cdc_new", "ng"):
            w[k + "_per_ev"] = w[k] / w["events"]
        wins.setdefault(w["kind"], []).append(w)
    report = {}
    tops = []
    for kind, ws in wins.items():
        rep = {"windows": len(ws)}
        rep["dist"] = {s: quantiles([w[s] for w in ws]) for s in SIGNALS}
        core = ["events", "zs", "zd", "cdc_new", "ng", "zs_per_ev", "errors", "err_bits", "unusual_bits"]
        rep["spearman"] = {a: {b: round(spearman([w[a] for w in ws], [w[b] for w in ws]), 2) for b in core}
                           for a in core}
        busy = [w for w in ws if w["events"] >= MIN_EVENTS[kind]]
        rep["busy_windows"] = len(busy)
        # loops: per-event bits (ascending) of each estimator against the exact-repeat label
        base_rep = sum(w["repeat"] for w in busy) / max(1, sum(w["calls"] for w in busy))
        loop = {"repeat_frac_all_busy": round(base_rep, 3)}
        for est in ("zs", "zd", "cdc_new", "ng"):
            key = est + "_per_ev" if est != "zs" else "zs_per_ev"
            srt = sorted(busy, key=lambda w: w[key])
            for n in (20, 50):
                top = srt[:n]
                loop[f"{est}_low{n}_repeat_frac"] = round(
                    sum(w["repeat"] for w in top) / max(1, sum(w["calls"] for w in top)), 3)
            loop[f"{est}_spearman_vs_repeat_frac"] = round(
                spearman([w[key] for w in busy], [w["repeat_frac"] for w in busy]), 2)
            loop[f"{est}_spearman_vs_new_items_per_ev"] = round(
                spearman([w[key] for w in busy], [w["new_items"] / w["events"] for w in busy]), 2)
        rep["loops"] = loop
        # overlap of the top-20 novelty windows across estimators (total bits, descending)
        top_sets = {e: {(w["writer"], w["start"]) for w in sorted(ws, key=lambda w: -w[e])[:20]}
                    for e in ("zs", "zd", "cdc_new", "ng")}
        rep["novelty_top20_overlap_with_zs"] = {e: len(top_sets[e] & top_sets["zs"]) for e in top_sets}
        report[kind] = rep
        rankings = {
            "volume": sorted(ws, key=lambda w: -w["events"]),
            "novelty": sorted(ws, key=lambda w: -w["zs"]),
            "repetitive": sorted(busy, key=lambda w: w["zs_per_ev"]),
            "errors": sorted(ws, key=lambda w: -w["err_bits"]),
            "unusual": sorted(ws, key=lambda w: -w["unusual_bits"]),
        }
        for sig, lst in rankings.items():
            for i, w in enumerate(lst[:20]):
                tops.append({"kind": kind, "signal": sig, "rank": i + 1, "writer": w["writer"],
                             "start": w["start"], "end": w["end"], "first_id": w["first_id"],
                             "last_id": w["last_id"], "session": w.get("session"),
                             **{k: w[k] for k in ("events", "calls", "errors", "err_out", "err_fail", "zs",
                                                  "zs_per_ev", "density", "err_bits", "unusual_bits", "u_rate",
                                                  "u_mix", "u_err", "u_new", "u_nov", "repeat", "new_tool",
                                                  "new_files", "n_sessions")}})
    with open(dst, "w") as f:
        json.dump(report, f, indent=1)
    if tops_path:
        with open(tops_path, "w") as f:
            for t in tops:
                f.write(json.dumps(t) + "\n")


if __name__ == "__main__":
    main()
