"""Work phases: a hidden Markov model (a probabilistic finite-state machine)
decoded with Viterbi, against an optimal-partitioning baseline (PELT with a
KT/MDL segment cost) and fixed windows. No neural model, no LLM.

    nice -n 19 python3 -I phases.py feat.tsv.zst OUT.json [--iters 12] [--prior 2000] [--dump phases.jsonl]
        [--limit N]   (every k-th session, for quick runs)
        [--split YYYY-MM-DD]  learn on sessions starting before (default: the first 60%)
        [--betas 30,60,120]   penalties of the plug-in-cost PELT baseline
        [--switch 0,16,32] [--main-switch 16]   phase switch costs (bits) to sweep / to report states with

Observations: one per activity record of a session (no system context, no
echo), five discrete features:
- fam: tool family (common.FAMILIES, 14 values)
- err: 0 none, 1 error output, 2 failed call
- nov: zstd conditional bits of the record (features.py `zs`):
       <256, <1024, <8192, >=8192
- gap: seconds since the session's previous record: <5, <30, <300, >=300
- rep: tool call repeating a recent identical call (0 no, 1 yes, 2 not a call)
Emissions factorize over the features given the state (naive Bayes).

States (hand-set start, then MAP Baum-Welch with Dirichlet pseudo-counts
`--prior` per feature per state from the start values): explore, edit,
run, debug, stuck, idle. Costs are in bits: -log2 of transition and
emission probabilities.

Writes aggregate numbers only (OUT.json); `--dump` writes per-phase rows
(session, times, state, counts; no text) for local hand-checking.
"""
import json
import sys
import time
from collections import Counter
from math import lgamma, log, log2

from compression import zstd

sys.path.insert(0, __import__("os").path.dirname(__file__))
from common import FAM, FAMILIES  # noqa: E402

STATES = ["explore", "edit", "run", "debug", "stuck", "idle"]
S = len(STATES)
FEATS = [("fam", len(FAMILIES)), ("err", 3), ("nov", 4), ("gap", 4), ("rep", 3)]
LN2 = log(2)


def nov_bin(bits):
    return 0 if bits < 256 else 1 if bits < 1024 else 2 if bits < 8192 else 3


def gap_bin(sec):
    return 0 if sec < 5 else 1 if sec < 30 else 2 if sec < 300 else 3


def load(path):
    """session -> list of (obs tuple, ts_us, zs bits, err, writer)."""
    sess = {}
    last = {}
    with zstd.open(path, "rt", encoding="utf-8") as f:
        next(f)
        for line in f:
            x = line.rstrip("\n").split("\t")
            kind, echo = int(x[4]), int(x[16])
            if kind == 0 or echo:
                continue
            s, ts = x[3], int(x[1])
            prev = last.get(s)
            gap = (ts - prev) / 1e6 if prev is not None else 1e9
            last[s] = ts
            err = int(x[6])
            rep = int(x[15]) if kind == 4 else 2
            zs = int(x[8])
            obs = (int(x[5]), err, nov_bin(zs), gap_bin(gap), rep)
            sess.setdefault(s, []).append((obs, ts, zs, err, int(x[2])))
    return sess


def start_params():
    """Hand-set FSM: sticky phases, emissions describing each phase."""
    def fam(**w):
        v = [0.2] * len(FAMILIES)
        for k, p in w.items():
            v[FAM[k]] = p
        t = sum(v)
        return [a / t for a in v]

    E = {
        "explore": [fam(read=8, web=4, say=1, think=1), [0.92, 0.06, 0.02], [0.1, 0.25, 0.45, 0.2],
                    [0.5, 0.3, 0.15, 0.05], [0.45, 0.05, 0.5]],
        "edit": [fam(edit=8, read=2, think=1, say=1), [0.9, 0.06, 0.04], [0.1, 0.3, 0.5, 0.1],
                 [0.5, 0.3, 0.15, 0.05], [0.45, 0.05, 0.5]],
        "run": [fam(run=6, test=4, vcs=2, read=1), [0.85, 0.1, 0.05], [0.15, 0.35, 0.4, 0.1],
                [0.45, 0.3, 0.2, 0.05], [0.45, 0.05, 0.5]],
        "debug": [fam(test=4, run=4, edit=3, read=2, think=1), [0.5, 0.3, 0.2], [0.1, 0.3, 0.45, 0.15],
                  [0.45, 0.35, 0.15, 0.05], [0.42, 0.08, 0.5]],
        # stuck: no family preference; defined by repetition (low novelty, repeated calls)
        "stuck": [fam(), [0.75, 0.1, 0.15], [0.75, 0.2, 0.04, 0.01],
                  [0.4, 0.35, 0.2, 0.05], [0.15, 0.35, 0.5]],
        "idle": [fam(wait=4, user=4, ask=3, say=3, delegate=3), [0.9, 0.05, 0.05], [0.3, 0.4, 0.25, 0.05],
                 [0.15, 0.2, 0.3, 0.35], [0.25, 0.05, 0.7]],
    }
    emit = [E[s] for s in STATES]
    trans = [[0.9 if i == j else 0.1 / (S - 1) for j in range(S)] for i in range(S)]
    init = [1.0 / S] * S
    return init, trans, emit


def emissions(emit, o):
    out = []
    for s in range(S):
        p = 1.0
        for f, v in enumerate(o):
            p *= emit[s][f][v]
        out.append(p)
    return out


def baum_welch(seqs, init, trans, emit, iters, prior):
    """MAP EM; pseudo-counts = prior x start emissions (anchors the meaning
    of each state) and prior/10 x start transitions."""
    p_emit = [[[prior * p for p in emit[s][f]] for f in range(len(FEATS))] for s in range(S)]
    p_trans = [[prior / 10 * p for p in trans[i]] for i in range(S)]
    ll_hist = []
    for it in range(iters):
        c_init = [1.0] * S
        c_trans = [row[:] for row in p_trans]
        c_emit = [[row[:] for row in st] for st in p_emit]
        ll = 0.0
        for seq in seqs:
            T = len(seq)
            B = [emissions(emit, o) for o in seq]
            alpha = []
            scale = []
            a = [init[s] * B[0][s] for s in range(S)]
            z = sum(a) or 1e-300
            a = [x / z for x in a]
            alpha.append(a)
            scale.append(z)
            for t in range(1, T):
                prev = alpha[-1]
                bt = B[t]
                a = [sum(prev[i] * trans[i][j] for i in range(S)) * bt[j] for j in range(S)]
                z = sum(a) or 1e-300
                a = [x / z for x in a]
                alpha.append(a)
                scale.append(z)
            ll += sum(log2(z) for z in scale)
            beta = [1.0] * S
            for t in range(T - 1, -1, -1):
                g = [alpha[t][s] * beta[s] for s in range(S)]
                zg = sum(g) or 1e-300
                o = seq[t]
                for s in range(S):
                    gs = g[s] / zg
                    if gs > 1e-9:
                        for f, v in enumerate(o):
                            c_emit[s][f][v] += gs
                if t == 0:
                    for s in range(S):
                        c_init[s] += g[s] / zg
                    break
                bt = B[t]
                w = [bt[j] * beta[j] for j in range(S)]
                ap = alpha[t - 1]
                st = scale[t]
                for i in range(S):
                    ai = ap[i] / st
                    if ai > 1e-12:
                        ti = trans[i]
                        ci = c_trans[i]
                        for j in range(S):
                            ci[j] += ai * ti[j] * w[j]
                beta = [sum(trans[i][j] * w[j] for j in range(S)) / st for i in range(S)]
        init = [c / sum(c_init) for c in c_init]
        trans = [[c / sum(row) for c in row] for row in c_trans]
        emit = [[[c / sum(row) for c in row] for row in st] for st in c_emit]
        ll_hist.append(round(ll, 1))
        print(f"iter {it + 1}: log2 L = {ll:.0f}", file=sys.stderr)
    return init, trans, emit, ll_hist


def viterbi(seq, init, trans, emit, switch_bits=0.0):
    """Most likely state path; costs in bits. `switch_bits` is a path cost
    per phase: every change of state pays it on top of -log2 transition."""
    lt = [[-log2(max(p, 1e-300)) + (switch_bits if i != j else 0.0) for j, p in enumerate(row)]
          for i, row in enumerate(trans)]
    le = [[[-log2(max(p, 1e-300)) for p in row] for row in st] for st in emit]

    def ecost(s, o):
        return sum(le[s][f][v] for f, v in enumerate(o))

    cost = [-log2(max(init[s], 1e-300)) + ecost(s, seq[0]) for s in range(S)]
    back = []
    for o in seq[1:]:
        nc, bp = [], []
        for j in range(S):
            best, arg = min((cost[i] + lt[i][j], i) for i in range(S))
            nc.append(best + ecost(j, o))
            bp.append(arg)
        back.append(bp)
        cost = nc
    s = min(range(S), key=lambda k: cost[k])
    path = [s]
    for bp in reversed(back):
        s = bp[s]
        path.append(s)
    path.reverse()
    return path, min(cost)


# ------------------------------------------------------------------ baseline: PELT with KT code length

def kt_bits(counts, n, k):
    """Code length (bits) of a sequence with these counts under the
    Krichevsky-Trofimov mixture (Dirichlet 1/2): parameter cost included."""
    v = lgamma(k / 2) - lgamma(n + k / 2) + sum(lgamma(c + 0.5) - lgamma(0.5) for c in counts if c)
    return -v / LN2


def plugin_bits(counts, n, k):
    """Plug-in (maximum-likelihood) code length n H(p) in bits: no
    parameter cost, so the penalty alone sets the number of segments."""
    return -sum(c * log2(c / n) for c in counts if c)


def pelt(seq, penalty, max_len=3000, seg_cost=kt_bits):
    """Optimal partitioning of a session into segments minimizing the sum of
    segment code lengths (per feature) + penalty per segment (PELT
    pruning; segments of at most max_len events)."""
    T = len(seq)
    nf = len(FEATS)
    cum = [[[0] * k for _, k in FEATS]]
    for o in seq:
        nxt = [row[:] for row in cum[-1]]
        for f, v in enumerate(o):
            nxt[f][v] += 1
        cum.append(nxt)

    def cost(a, b):
        n = b - a
        return sum(seg_cost([cum[b][f][v] - cum[a][f][v] for v in range(FEATS[f][1])], n, FEATS[f][1])
                   for f in range(nf))

    F = [0.0] * (T + 1)
    F[0] = -penalty
    last = [0] * (T + 1)
    cand = [0]
    for t in range(1, T + 1):
        vals = [(F[s] + cost(s, t) + penalty, s) for s in cand if t - s <= max_len]
        if not vals:
            vals = [(F[t - 1] + cost(t - 1, t) + penalty, t - 1)]
        F[t], last[t] = min(vals)
        cand = [s for (v, s) in vals if v - penalty <= F[t]] + [t]
    bounds = []
    t = T
    while t > 0:
        bounds.append(t)
        t = last[t]
    return sorted(bounds)  # segment ends


# ------------------------------------------------------------------ evaluation

def r2(values, seg_ids):
    """Share of the variance of a per-event value explained by segment means."""
    n = len(values)
    mean = sum(values) / n
    tot = sum((x - mean) ** 2 for x in values)
    if tot == 0:
        return None
    by = {}
    for x, s in zip(values, seg_ids):
        a = by.setdefault(s, [0.0, 0])
        a[0] += x
        a[1] += 1
    within = sum((x - by[s][0] / by[s][1]) ** 2 for x, s in zip(values, seg_ids))
    return 1 - within / tot


def main():
    a = sys.argv[1:]
    src, dst = a[0], a[1]
    opt = dict(zip(a[2::2], a[3::2]))
    iters = int(opt.get("--iters", 12))
    prior = float(opt.get("--prior", 2000))
    dump = opt.get("--dump")
    t0 = time.perf_counter()
    sess = load(src)
    seqs_all = {s: v for s, v in sess.items() if len(v) >= 8}
    load_s = time.perf_counter() - t0
    names = sorted(seqs_all)
    if "--limit" in opt:  # quick runs: every k-th session
        k = max(1, len(names) // int(opt["--limit"]))
        names = names[::k]
    obs = [[e[0] for e in seqs_all[s]] for s in names]
    T = sum(len(o) for o in obs)
    print(f"{len(obs)} sessions, {T} events", file=sys.stderr)
    init, trans, emit = start_params()
    # hand-set FSM decode (no learning), for comparison
    paths_hand = [viterbi(o, init, trans, emit)[0] for o in obs]
    # learn on sessions that start before the split, evaluate on all and on the later ones
    split = opt.get("--split")
    if split:
        from datetime import datetime, timezone
        cut = int(datetime.fromisoformat(split).replace(tzinfo=timezone.utc).timestamp() * 1e6)
    else:
        cut = sorted(seqs_all[s][0][1] for s in names)[int(len(names) * 0.6)]
    train = [o for s, o in zip(names, obs) if seqs_all[s][0][1] < cut]
    held = {s for s in names if seqs_all[s][0][1] >= cut}
    print(f"train {len(train)} sessions; held out {len(held)}", file=sys.stderr)
    t1 = time.perf_counter()
    init, trans, emit, ll = baum_welch(train, init, trans, emit, iters, prior)
    t_bw = (time.perf_counter() - t1) / max(iters, 1)
    switches = [float(x) for x in opt.get("--switch", "0,16,32").split(",")]
    main_switch = float(opt.get("--main-switch", 16))
    t2 = time.perf_counter()
    paths = [viterbi(o, init, trans, emit, main_switch)[0] for o in obs]
    t_vit = time.perf_counter() - t2
    sweep = {sw: [viterbi(o, init, trans, emit, sw)[0] for o in obs] for sw in switches if sw != main_switch}
    sweep[main_switch] = paths
    # baselines
    t3 = time.perf_counter()
    pelt_bounds = [pelt(o, penalty=log2(max(len(o), 2))) for o in obs]  # MDL: KT cost + boundary position
    t_pelt = time.perf_counter() - t3
    plug = {}
    for beta in [float(x) for x in opt.get("--betas", "30,60,120").split(",")]:
        plug[beta] = [pelt(o, penalty=beta, seg_cost=plugin_bits) for o in obs]

    # per-event values for R^2: novelty (log2 bits), error flag, log gap, repeat flag
    out = {"sessions": len(obs), "events": T, "states": STATES, "iters": iters, "prior": prior,
           "main_switch_bits": main_switch,
           "log2_likelihood_by_iter": ll,
           "seconds": {"load": round(load_s, 1), "baum_welch_per_iter": round(t_bw, 1),
                       "viterbi_all": round(t_vit, 1), "pelt_all": round(t_pelt, 1)},
           "us_per_event": {"viterbi": round(t_vit / T * 1e6, 2), "baum_welch_iter": round(t_bw / T * 1e6, 2),
                            "pelt": round(t_pelt / T * 1e6, 2)},
           "transitions": [[round(p, 4) for p in row] for row in trans],
           "emissions": {STATES[s]: {FEATS[f][0]: [round(p, 3) for p in emit[s][f]] for f in range(len(FEATS))}
                         for s in range(S)}}

    def segs_from_path(p):
        ids, k = [], 0
        for i, s in enumerate(p):
            if i and s != p[i - 1]:
                k += 1
            ids.append(k)
        return ids

    def segs_from_bounds(b, n):
        ids, k, j = [], 0, 0
        for i in range(n):
            while j < len(b) and i >= b[j]:
                j += 1
                k += 1
            ids.append(k)
        return ids

    def segs_fixed(ts, size_us):
        return [t // size_us for t in ts]

    keys = ["hmm_hand", "pelt", "fixed_5m", "fixed_1h"] + [f"hmm_sw{sw:g}" for sw in switches] + \
        [f"equal_events_as_hmm_sw{sw:g}" for sw in switches] + [f"pelt_plugin_{b:g}" for b in plug]
    nsegs = Counter()
    allv = {"nov": [], "err": [], "gap": [], "rep": []}
    is_held = []
    seg_global = {k: [] for k in keys}
    for idx, s in enumerate(names):
        ev = seqs_all[s]
        ts = [e[1] for e in ev]
        n = len(ev)
        segs = {
            "hmm_hand": segs_from_path(paths_hand[idx]),
            "pelt": segs_from_bounds(pelt_bounds[idx], n),
            "fixed_5m": segs_fixed(ts, 300_000_000),
            "fixed_1h": segs_fixed(ts, 3_600_000_000),
        }
        for sw in switches:
            hs = segs_from_path(sweep[sw][idx])
            segs[f"hmm_sw{sw:g}"] = hs
            k = hs[-1] + 1  # the same number of segments, each with the same number of events
            segs[f"equal_events_as_hmm_sw{sw:g}"] = [i * k // n for i in range(n)]
        for b, bb in plug.items():
            segs[f"pelt_plugin_{b:g}"] = segs_from_bounds(bb[idx], n)
        for k, v in segs.items():
            nsegs[k] += len(set(v))
            seg_global[k].extend((idx, x) for x in v)
        prev = None
        for e in ev:
            is_held.append(s in held)
            allv["nov"].append(log2(e[2] + 1))
            allv["err"].append(1.0 if e[3] else 0.0)
            gap = (e[1] - prev) / 1e6 if prev is not None else 0.0
            allv["gap"].append(log2(gap + 1))
            prev = e[1]
            allv["rep"].append(1.0 if e[0][4] == 1 else 0.0)
    out["segments"] = dict(nsegs)
    out["r2_within_session"] = {k: {v: round(r2(allv[v], seg_global[k]), 3) for v in allv} for k in keys}
    hv = {v: [x for x, h in zip(allv[v], is_held) if h] for v in allv}
    out["r2_held_out_sessions"] = {
        k: {v: round(r2(hv[v], [g for g, h in zip(seg_global[k], is_held) if h]), 3) for v in allv} for k in keys}
    out["segments_held_out"] = {k: len({g for g, h in zip(seg_global[k], is_held) if h}) for k in keys}

    # what each state holds
    st_counts = Counter()
    st_rep = Counter()
    st_err = Counter()
    st_calls = Counter()
    st_fam = {s: Counter() for s in range(S)}
    phase_len = {s: [] for s in range(S)}
    rows = []
    for idx, s in enumerate(names):
        ev = seqs_all[s]
        p = paths[idx]
        start = 0
        for i in range(1, len(p) + 1):
            if i == len(p) or p[i] != p[start]:
                st = p[start]
                seg = ev[start:i]
                phase_len[st].append((seg[-1][1] - seg[0][1]) / 60e6)
                rows.append({"session": s, "writer": seg[0][4], "state": STATES[st], "start": seg[0][1],
                             "end": seg[-1][1], "events": len(seg),
                             "errors": sum(1 for e in seg if e[3]),
                             "repeats": sum(1 for e in seg if e[0][4] == 1),
                             "calls": sum(1 for e in seg if e[0][4] != 2),
                             "bits": sum(e[2] for e in seg)})
                start = i
        for e, st in zip(ev, p):
            st_counts[st] += 1
            st_err[st] += bool(e[3])
            st_fam[st][FAMILIES[e[0][0]]] += 1
            if e[0][4] != 2:
                st_calls[st] += 1
                st_rep[st] += e[0][4] == 1
    out["by_state"] = {
        STATES[s]: {"events": st_counts[s], "share": round(st_counts[s] / T, 3),
                    "phases": len(phase_len[s]),
                    "median_phase_min": round(sorted(phase_len[s])[len(phase_len[s]) // 2], 2) if phase_len[s] else 0,
                    "error_rate": round(st_err[s] / max(1, st_counts[s]), 3),
                    "repeat_rate_of_calls": round(st_rep[s] / max(1, st_calls[s]), 3),
                    "top_families": dict(st_fam[s].most_common(4))}
        for s in range(S)}
    tot_rep = sum(st_rep.values())
    out["repeats_in_stuck_share"] = round(st_rep[STATES.index("stuck")] / max(1, tot_rep), 3)
    out["stuck_event_share"] = round(st_counts[STATES.index("stuck")] / T, 3)
    with open(dst, "w") as f:
        json.dump(out, f, indent=1)
    if dump:
        with open(dump, "w") as f:
            for r in rows:
                f.write(json.dumps(r) + "\n")
    print(json.dumps({k: out[k] for k in ("segments", "r2_within_session", "segments_held_out",
                                           "r2_held_out_sessions", "repeats_in_stuck_share",
                                           "stuck_event_share", "seconds", "us_per_event")}, indent=1),
          file=sys.stderr)


if __name__ == "__main__":
    main()
