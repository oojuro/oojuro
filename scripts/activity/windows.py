"""Windows (5 min, 1 h per writer; sessions) and their four signals, from
the per-record features of `features.py`.

    nice -n 19 python3 -I windows.py feat.tsv.zst windows.jsonl [--warm-until YYYY-MM-DD]

Window kinds: `5m`, `1h` (one writer's clock-aligned window, all its
sessions), `5m/s`, `1h/s` (the same, one session), `session` (a whole
session). Each output line is one active window (numbers only):
- volume: `events` (activity records: no system context, no echo),
  `calls`, `results`
- errors: `err_out` (error output), `err_fail` (failed calls), `errors`
  (results with either), `err_bits` (upper-tail binomial surprisal of the
  error count against the writer's trailing 7-day error rate)
- novelty: `zs`, `zd`, `ng` bits, `cdc_new` bytes; `zs_per_ev` bits per
  event (low = repetitive)
- change vs normal: `unusual_bits` = sum of components `u_rate`, `u_mix`,
  `u_err`, `u_new`, `u_nov` (each >= 0), all against the writer's trailing
  7 days (windows of the same kind; sessions: earlier sessions); exposure
  capped at N_REF events so the components measure shape, not size
- labels for checking: `repeat` (calls repeating a recent identical call)
"""
import json
import sys
from bisect import bisect_left, bisect_right, insort
from collections import deque
from math import exp, lgamma, log, log2

from compression import zstd

SIZES = {"5m": 300_000_000, "1h": 3_600_000_000}
# the same sizes per session ("5m/s": one session's 5 minutes)
BASE = 7 * 86_400_000_000
NFAM = 14
ACT_FAMS = 10  # tool families (read..other); user/say/think/note are not tools
ALPHA = 0.5  # Dirichlet smoothing of the baseline family mix
N_REF = 50  # exposure cap (events) of the unusual components
LN2 = log(2)


def new_acc(start, end):
    return {"start": start, "end": end, "events": 0, "calls": 0, "results": 0, "err_out": 0, "err_fail": 0,
            "errors": 0, "bytes": 0, "zs": 0, "zd": 0, "cdc_new": 0, "ng": 0.0, "ntok": 0, "new_tool": 0,
            "new_files": 0, "repeat": 0, "sys": 0, "echo": 0, "fam": [0] * NFAM, "first_id": 0, "last_id": 0,
            "t0": None, "t1": None, "sessions": set()}


def add(a, row):
    (rid, ts, _w, sess, kind, fam, err, nbytes, zs, zd, cdc, ng, ntok, nt, nf, rep, echo) = row
    a["t0"] = ts if a["t0"] is None else a["t0"]
    a["t1"] = ts
    a["first_id"] = a["first_id"] or rid
    a["last_id"] = rid
    if err:
        if kind == 5:
            a["errors"] += 1
        if err == 2:
            a["err_fail"] += 1
        else:
            a["err_out"] += 1
    if echo:
        a["echo"] += 1
        return
    if kind == 0:
        a["sys"] += 1
        return
    a["events"] += 1
    a["sessions"].add(sess)
    a["fam"][fam] += 1
    a["calls"] += kind == 4
    a["results"] += kind == 5
    a["bytes"] += nbytes
    a["zs"] += zs
    a["zd"] += zd
    a["cdc_new"] += cdc
    a["ng"] += ng
    a["ntok"] += ntok
    a["new_tool"] += nt
    a["new_files"] += nf
    a["repeat"] += rep


def parse(line):
    f = line.rstrip("\n").split("\t")
    return (int(f[0]), int(f[1]), int(f[2]), f[3], int(f[4]), int(f[5]), int(f[6]), int(f[7]), int(f[8]),
            int(f[9]), int(f[10]), float(f[11]), int(f[12]), int(f[13]), int(f[14]), int(f[15]), int(f[16]))


# ------------------------------------------------------------------ tails in bits

def binom_upper_bits(k, n, p):
    """-log2 P(K >= k) for K ~ Binomial(n, p); 0 when k <= n p."""
    if k <= n * p or k == 0:
        return 0.0
    lp, lq = log(p), log(1 - p)
    logpmf = lambda i: lgamma(n + 1) - lgamma(i + 1) - lgamma(n - i + 1) + i * lp + (n - i) * lq  # noqa: E731
    first = logpmf(k)
    total = 1.0
    for i in range(k + 1, n + 1):
        t = exp(logpmf(i) - first)
        total += t
        if t < 1e-12 * total:
            break
    return max(0.0, -(first + log(total)) / LN2)


def mix_bits(counts, base, base_total, n_ref=None):
    """Excess code length of the window's family mix under the baseline mix
    (Dirichlet-smoothed) over its own mix, minus the parameter cost
    ((K-1)/2) log2 n of describing its own mix (MDL); >= 0. With `n_ref`,
    the exposure is capped at n_ref events (shape, not size)."""
    n = sum(counts)
    if n < 2:
        return 0.0
    k_obs = sum(1 for c in counts if c)
    denom = base_total + ALPHA * ACT_FAMS
    kl = 0.0
    for c, b in zip(counts, base):
        if c:
            kl += (c / n) * log2((c / n) / ((b + ALPHA) / denom))
    ne = min(n, n_ref) if n_ref else n
    return max(0.0, ne * kl - (k_obs - 1) / 2 * log2(ne))


def bern_kl_bits(k, n, p0, n_ref):
    """Evidence (bits) that a rate k/n exceeds p0, exposure capped at n_ref."""
    if n == 0 or k / n <= p0:
        return 0.0
    q = min(k / n, 1 - 1e-9)
    kl = q * log2(q / p0) + (1 - q) * log2((1 - q) / (1 - p0))
    return min(n, n_ref) * kl


def poisson_kl_bits(x, n, rate0, n_ref):
    """Evidence (bits) that x events in exposure n exceed rate0 per unit,
    exposure capped at n_ref."""
    if n == 0 or x / n <= rate0:
        return 0.0
    r = x / n
    ne = min(n, n_ref)
    return ne * (r * log2(r / rate0) - (r - rate0) / LN2)


def tail_bits_two_sided(x, sorted_vals):
    """-log2 of the two-sided empirical tail of x among baseline values (+1 smoothing)."""
    n = len(sorted_vals)
    if n < 20:
        return 0.0
    lo = bisect_right(sorted_vals, x)
    hi = n - bisect_left(sorted_vals, x)
    p = min(1.0, 2 * (min(lo, hi) + 1) / (n + 1))
    return -log2(p)


# ------------------------------------------------------------------ baselines

class Baseline:
    """A writer's trailing windows of one kind over the last 7 days.

    Change vs normal has two steps. A window's raw statistic per dimension
    is computed against the baseline's pooled counts (`raw_*`: event count
    or rate, family-mix evidence, error-rate evidence, new-item evidence,
    novelty density). Then each statistic is calibrated to its rarity among
    the same statistics of the baseline windows: u = -log2 P(stat >= x),
    empirical with +1 smoothing (two-sided for density), empty time slots
    counted as zero. Every component is then in comparable bits (at most
    log2 of the baseline size), and `unusual_bits` is their sum."""

    DIMS = ("rate", "mix", "err", "new", "nov")

    def __init__(self, active_only=False):
        self.active_only = active_only  # sessions, session windows: no empty slots
        self.q = deque()
        self.s = {"events": 0, "results": 0, "errors": 0, "new": 0}
        self.fam = [0] * NFAM
        self.raw = {d: [] for d in self.DIMS}

    def push(self, w):
        self.q.append(w)
        self.s["events"] += w["events"]
        self.s["results"] += w["results"]
        self.s["errors"] += w["errors"]
        self.s["new"] += w["new_tool"] + w["new_files"]
        for i, c in enumerate(w["fam"]):
            self.fam[i] += c
        for d in self.DIMS:
            if w.get("raw", {}).get(d) is not None:
                insort(self.raw[d], w["raw"][d])

    def expire(self, now):
        while self.q and self.q[0]["start"] < now - BASE:
            w = self.q.popleft()
            self.s["events"] -= w["events"]
            self.s["results"] -= w["results"]
            self.s["errors"] -= w["errors"]
            self.s["new"] -= w["new_tool"] + w["new_files"]
            for i, c in enumerate(w["fam"]):
                self.fam[i] -= c
            for d in self.DIMS:
                x = w.get("raw", {}).get(d)
                if x is not None:
                    del self.raw[d][bisect_left(self.raw[d], x)]

    def score(self, w, slots):
        """Signals of window `w`; stores its raw statistics in w["raw"].
        `slots`: windows the baseline span holds, empty ones included (time
        windows), else the number of baseline windows.

        `err_bits` (the error signal) is evidence with full exposure: more
        errors at a higher rate rank higher."""
        s = self.s
        tot = sum(self.fam[:ACT_FAMS])
        p0 = (s["errors"] + 1) / (s["results"] + 2)
        rate0 = (s["new"] + 1) / (s["events"] + 1)
        raw = {
            "rate": rate_of(w) if self.active_only else float(w["events"]),
            "mix": mix_bits(w["fam"][:ACT_FAMS], self.fam[:ACT_FAMS], tot, N_REF),
            "err": bern_kl_bits(w["errors"], w["results"], p0, N_REF),
            "new": poisson_kl_bits(w["new_tool"] + w["new_files"], w["events"], rate0, N_REF),
            "nov": w["zs"] / (8 * w["bytes"]) if w["bytes"] >= 256 else None,
        }
        w["raw"] = raw
        n_slots = len(self.q) if self.active_only else slots
        out = {"err_bits": round(binom_upper_bits(w["errors"], w["results"], p0), 2)}
        total = 0.0
        for d in self.DIMS:
            x, vals = raw[d], self.raw[d]
            if x is None or len(self.q) < 20:
                u = 0.0
            elif d == "nov":
                u = tail_bits_two_sided(x, vals)
            elif x <= 0:
                u = 0.0
            else:  # baseline windows with a smaller value, and empty slots, are below x
                hi = len(vals) - bisect_left(vals, x)
                u = -log2(min(1.0, (hi + 1) / (max(n_slots, len(vals)) + 1)))
            out["u_" + d] = round(u, 2)
            total += u
        out["unusual_bits"] = round(total, 2)
        out.update({"raw_" + d: (round(v, 3) if v is not None else None) for d, v in raw.items()})
        return out


def rate_of(w):
    """Events per minute over the window's active span (at least a minute)."""
    return w["events"] / max(1.0, ((w["t1"] or 0) - (w["t0"] or 0)) / 60e6)


def finish(w, kind, writer):
    out = {k: v for k, v in w.items() if k not in ("sessions", "raw")}
    out["kind"] = kind
    out["writer"] = writer
    out["n_sessions"] = len(w["sessions"])
    out["ng"] = round(w["ng"], 1)
    out["zs_per_ev"] = round(w["zs"] / w["events"], 1) if w["events"] else 0.0
    out["density"] = round(w["zs"] / (8 * w["bytes"]), 4) if w["bytes"] else 0.0
    return out


def main():
    src, dst = sys.argv[1], sys.argv[2]
    warm_until = None
    if "--warm-until" in sys.argv:  # windows before this date only feed baselines
        from datetime import datetime, timezone
        d = datetime.fromisoformat(sys.argv[sys.argv.index("--warm-until") + 1]).replace(tzinfo=timezone.utc)
        warm_until = int(d.timestamp() * 1e6)
    cur = {}  # (writer, size) -> acc
    done = {k: [] for k in SIZES}
    scur = {}  # (session, size) -> (writer, acc)
    sdone = {k: [] for k in SIZES}
    sessions = {}  # session -> (writer, acc)
    with zstd.open(src, "rt", encoding="utf-8") as f:
        next(f)
        for line in f:
            row = parse(line)
            ts, w = row[1], row[2]
            for name, size in SIZES.items():
                b = ts // size * size
                a = cur.get((w, name))
                if a is None or a["start"] != b:
                    if a is not None and (a["events"] or a["errors"] or a["sys"]):
                        done[name].append((w, a))
                    a = cur[(w, name)] = new_acc(b, b + size)
                add(a, row)
                sa = scur.get((row[3], name))
                if sa is None or sa[1]["start"] != b:
                    if sa is not None and sa[1]["events"]:
                        sdone[name].append(sa)
                    sa = scur[(row[3], name)] = (w, new_acc(b, b + size))
                add(sa[1], row)
            s = sessions.get(row[3])
            if s is None:
                s = sessions[row[3]] = (w, new_acc(ts, ts))
            add(s[1], row)
    for (w, name), a in cur.items():
        if a["events"] or a["errors"] or a["sys"]:
            done[name].append((w, a))
    for (sess, name), (w, a) in scur.items():
        if a["events"]:
            sdone[name].append((w, a))
    out = open(dst, "w")
    t_min = min(a["start"] for v in done.values() for _, a in v)
    warm_until = warm_until or t_min + 3 * 86_400_000_000
    for name, size in SIZES.items():
        slots = BASE // size
        per_writer = {}
        for w, a in sorted(done[name], key=lambda x: (x[1]["start"], x[0])):
            per_writer.setdefault(w, []).append(a)
        for w, wins in per_writer.items():
            base = Baseline()
            for a in wins:
                base.expire(a["start"])
                if a["events"]:
                    rec = finish(a, name, w)
                    rec.update(base.score(a, slots))
                    rec["warm"] = a["start"] < warm_until
                    out.write(json.dumps(rec) + "\n")
                base.push(a)
    # session windows: baseline = the writer's active session windows (7 days)
    for name, size in SIZES.items():
        per_writer = {}
        for w, a in sorted(sdone[name], key=lambda x: (x[1]["start"], x[0])):
            per_writer.setdefault(w, []).append(a)
        for w, wins in per_writer.items():
            base = Baseline(active_only=True)
            for a in wins:
                base.expire(a["start"])
                rec = finish(a, name + "/s", w)
                rec["session"] = next(iter(a["sessions"]))
                n = max(len(base.q), 1)
                rec.update(base.score(a, n))
                rec["warm"] = a["start"] < warm_until
                out.write(json.dumps(rec) + "\n")
                base.push(a)
    # sessions: baseline = the writer's earlier sessions in the trailing 7 days
    by_writer = {}
    for sid, (w, a) in sessions.items():
        a["start"], a["end"] = a["t0"], a["t1"]
        by_writer.setdefault(w, []).append((sid, a))
    for w, ss in by_writer.items():
        ss.sort(key=lambda x: x[1]["start"])
        base = Baseline(active_only=True)
        for sid, a in ss:
            base.expire(a["start"])
            if a["events"]:
                rec = finish(a, "session", w)
                rec["session"] = sid
                n = max(len(base.q), 1)
                rec.update(base.score(a, n))
                rec["warm"] = a["start"] < warm_until
                out.write(json.dumps(rec) + "\n")
            base.push(a)
    out.close()


if __name__ == "__main__":
    main()
