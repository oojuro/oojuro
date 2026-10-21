"""Validate a TBEV0001 file: dense ids, parent/revises references, per-session order."""
import struct, sys
EV = struct.Struct("<QQIIIQQBBiIIqqHII")
f = open(sys.argv[1], "rb")
assert f.read(8) == b"TBEV0001"
nxt, kinds, last_seq, last_ts, n, batches = 1, {}, {}, {}, 0, 0
tools = {}
while True:
    hd = f.read(4)
    if not hd:
        break
    (m,) = struct.unpack("<I", hd)
    batches += 1
    sess = set()
    for _ in range(m):
        e = EV.unpack(f.read(EV.size))
        tool, args, text = f.read(e[14]), f.read(e[15]), f.read(e[16])
        id_, s, ag, turn, seq, parent, rev, kind, status, ex, tin, tout, ts, dur = e[:14]
        assert id_ == nxt, (id_, nxt); nxt += 1
        assert parent < id_ and (parent == 0 or parent in kinds), (id_, parent)
        if rev:
            assert kinds[rev] == 4 and kind == 4 and not text, id_
        assert seq == last_seq.get(s, -1) + 1, (id_, s, seq)
        last_seq[s] = seq
        assert ts >= last_ts.get(s, 0) - 1, (id_, ts)
        last_ts[s] = max(ts, last_ts.get(s, 0))
        kinds[id_] = kind
        sess.add(s)
        text.decode("utf-8")
        if kind == 4 and not rev:
            tools[tool] = tools.get(tool, 0) + 1
        n += 1
    assert len(sess) == 1, "a batch belongs to one session"
print({"events": n, "batches": batches, "sessions": len(last_seq), "top_tools": sorted(tools.items(), key=lambda x: -x[1])[:12]})
