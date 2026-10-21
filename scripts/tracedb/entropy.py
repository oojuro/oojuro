"""Information content of a TraceBench tier-1 stream (Section D: storage vs entropy).

1. Writes the text stream (tool + args + text of every event) in two orders,
   canonical append order (agents interleaved) and session order, for the
   whole-stream compressor floors run by entropy_floors.sh.
2. Empirical entropy of every typed field under simple conditional models,
   i.e. how many bits per event the metadata really carries.

Usage: python -I entropy.py <events.tbe> <out_dir>
Prints one JSON object; writes <out_dir>/text.append and text.session.
"""
import collections, json, math, os, struct, sys

EV = struct.Struct("<QQIIIQQBBiIIqqHII")


def H(counter):
    n = sum(counter.values())
    return -sum(c / n * math.log2(c / n) for c in counter.values() if c) if n else 0.0


def Hcond(pairs):
    """H(Y | X) in bits from a Counter of (x, y)."""
    by_x = collections.defaultdict(collections.Counter)
    for (x, y), c in pairs.items():
        by_x[x][y] += c
    n = sum(pairs.values())
    return sum(sum(cy.values()) / n * H(cy) for cy in by_x.values())


def zz(v):
    return v.bit_length() if v >= 0 else (-v).bit_length() + 1


def main():
    path, out = sys.argv[1], sys.argv[2]
    os.makedirs(out, exist_ok=True)
    f = open(path, "rb")
    assert f.read(8) == b"TBEV0001"
    fa = open(os.path.join(out, "text.append"), "wb", buffering=1 << 22)
    sessions = collections.defaultdict(list)  # session -> [(offset, len)] in the append file
    off = 0
    c = {k: collections.Counter() for k in ("kind_prev", "tool_kind", "status_kind", "exit_status",
                                            "parent_delta", "ts_delta_bits", "seq_delta", "turn_delta",
                                            "session_switch", "agent", "tin_minus_model", "tout_minus_model",
                                            "dur_bits", "revises_delta")}
    last = {}  # session -> (kind, ts, seq, turn, id)
    prev_session_by_agent = {}
    n = n_orig = 0
    while True:
        hd = f.read(4)
        if not hd:
            break
        (m,) = struct.unpack("<I", hd)
        for _ in range(m):
            e = EV.unpack(f.read(EV.size))
            tool, args, text = f.read(e[14]), f.read(e[15]), f.read(e[16])
            id_, s, ag, turn, seq, parent, rev, kind, status, ex, tin, tout, ts, dur = e[:14]
            blob = tool + args + text
            fa.write(blob)
            sessions[s].append((off, len(blob)))
            off += len(blob)
            n += 1
            pk, pts, pseq, pturn, pid = last.get(s, (255, ts, -1, 0, id_))
            c["kind_prev"][(pk, kind if not rev else 9)] += 1
            if rev:
                c["revises_delta"][(0, zz(id_ - rev))] += 1
                c["dur_bits"][(0, zz(dur))] += 1
                c["exit_status"][(status, ex)] += 1
            else:
                n_orig += 1
                c["tool_kind"][(kind, tool)] += 1
                c["status_kind"][(kind, status)] += 1
                c["exit_status"][(status, ex)] += 1
                c["parent_delta"][(kind, zz(id_ - parent) if parent else -1)] += 1
                c["ts_delta_bits"][(kind, zz(ts - pts))] += 1
                # token estimates are a function of the text length (ceil(len/4)) and context
                c["tout_minus_model"][(0, tout - ((len(text) + 3) // 4 if kind in (2, 3, 4) else 0))] += 1
            c["seq_delta"][(0, seq - pseq)] += 1
            c["turn_delta"][(0, turn - pturn)] += 1
            prv = prev_session_by_agent.get(ag)
            c["session_switch"][(0, prv == s)] += 1
            prev_session_by_agent[ag] = s
            c["agent"][(0, ag)] += 1
            last[s] = (kind if not rev else pk, ts, seq, turn, id_)
    fa.close()
    # session-ordered copy
    src = open(os.path.join(out, "text.append"), "rb")
    with open(os.path.join(out, "text.session"), "wb", buffering=1 << 22) as fs:
        for s in sorted(sessions):
            for o, l in sessions[s]:
                src.seek(o)
                fs.write(src.read(l))
    res = {"events": n, "original_events": n_orig, "text_bytes": off}
    bits = {}
    for k, v in c.items():
        bits[k] = round(Hcond(v), 4)
    res["conditional_entropy_bits"] = bits
    # A model where: kind|prev kind, tool|kind, status/exit, parent delta|kind, ts delta (bit-length
    # class + that many raw bits), seq/turn deltas, which session (per agent: same as last or not +
    # log2 of active sessions on a switch is bounded by log2(sessions) here), tokens (residual vs
    # the length model; 0 when tokens are derived), revision fields.
    ts_mantissa = sum(cnt * max(0, b - 1) for (_, b), cnt in c["ts_delta_bits"].items()) / max(1, n_orig)
    dur_mantissa = sum(cnt * max(0, b - 1) for (_, b), cnt in c["dur_bits"].items()) / max(1, n - n_orig)
    switch_rate = c["session_switch"][(0, False)] / n
    per_event = (bits["kind_prev"] + bits["tool_kind"] * 1 + bits["exit_status"] + bits["parent_delta"]
                 + bits["ts_delta_bits"] + ts_mantissa * n_orig / n + bits["seq_delta"] + bits["turn_delta"]
                 + bits["session_switch"] + switch_rate * math.log2(max(2, len(sessions)))
                 + bits["tout_minus_model"] + (bits["dur_bits"] + dur_mantissa + bits["revises_delta"]) * (n - n_orig) / n)
    res["typed_fields_bits_per_event_upper"] = round(per_event, 2)
    res["typed_fields_bytes_total_upper"] = round(per_event * n / 8)
    res["sessions"] = len(sessions)
    print(json.dumps(res))


if __name__ == "__main__":
    main()
