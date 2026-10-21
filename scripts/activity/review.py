"""Summaries of top windows for hand-labelling (LOCAL ONLY: the output
quotes transcript text; never commit or paste it anywhere).

    python3 -I review.py tops.jsonl writers.json KIND SIGNAL [N] [--lines 12]

Per window: counts by family, distinct calls, the most repeated calls
(digits folded), the first error lines, and the first activity lines.
Reads records through `oojuro range` (read-only).
"""
import collections
import json
import re
import subprocess
import sys

sys.path.insert(0, __import__("os").path.dirname(__file__))
from common import Families, error_level, is_activity  # noqa: E402


def records(writer, start, end):
    cmd = ["oojuro", "--json", "range", f"@{start / 1e6:.6f}", f"@{(end - 1) / 1e6:.6f}", "--writer", writer,
           "--limit", "50000"]
    return json.loads(subprocess.run(cmd, capture_output=True, text=True, check=True).stdout)


def one_line(t, n):
    return " ".join(t.split())[:n]


def main():
    a = sys.argv[1:]
    tops, writers, kind, signal = a[0], a[1], a[2], a[3]
    n = int(a[4]) if len(a) > 4 and a[4].isdigit() else 20
    nlines = int(a[a.index("--lines") + 1]) if "--lines" in a else 12
    names = {v: k for k, v in json.load(open(writers)).items()}
    for line in open(tops):
        t = json.loads(line)
        if t["kind"] != kind or t["signal"] != signal or t["rank"] > n:
            continue
        w = names[t["writer"]]
        recs = records(w, t["start"], t["end"])
        if t.get("session") and "/s" in kind or kind == "session":
            recs = [r for r in recs if r["session"] == t["session"]]
        fams = Families()
        fam_c = collections.Counter()
        calls = collections.Counter()
        errs = []
        acts = []
        for r in recs:
            if r["kind"] == "system":
                if error_level(r):
                    errs.append("sys:" + one_line(r["text"], 100))
                continue
            f = fams.of(r)
            if not is_activity(r):
                continue
            fam_c[f] += 1
            if r["kind"] == "tool_call":
                calls[re.sub(r"\d+", "N", one_line(r["text"], 90))] += 1
            lvl = error_level(r)
            if lvl and len(errs) < 4:
                errs.append(f"e{lvl}:" + one_line(r["text"], 110))
            if len(acts) < nlines:
                acts.append(f"{r['ts'][11:19]} {r['kind'][:6]:6} {r.get('tool', '')[:10]:10} {f:6} | "
                            + one_line(r["text"], 100))
        dur = (t["end"] - t["start"]) / 60e6
        print(f"### #{t['rank']} {kind} {signal} w{t['writer']} {t['start']} ({dur:.0f} min) ev={t['events']} "
              f"calls={t['calls']} err={t['errors']} zs/ev={t['zs_per_ev']} dens={t['density']} "
              f"err_bits={t['err_bits']} unusual={t['unusual_bits']} (rate {t['u_rate']} mix {t['u_mix']} "
              f"err {t['u_err']} new {t['u_new']} nov {t['u_nov']}) repeat={t['repeat']} sessions={t['n_sessions']}")
        print("  families:", dict(fam_c.most_common()))
        print(f"  distinct calls: {len(calls)}/{sum(calls.values())}")
        for c, k in calls.most_common(3):
            print(f"  x{k}: {c}")
        for e in errs:
            print("  ", e)
        for x in acts:
            print("   ", x)


if __name__ == "__main__":
    main()
