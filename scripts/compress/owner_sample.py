"""Owner-journal sample for the codec leaderboard (never committed: data
stays under .scratch). Reads `oojuro export` JSON lines on stdin, keeps
whole sessions chosen in a fixed hash order (sha256 of the session id)
until at least TARGET bytes of text are kept, and writes them in append
(id) order as an OJCORP01 corpus (crates/oojuro-store/src/corpus.rs).
Prints aggregate counts only, never text.

  oojuro export --from 60d | python3 -I owner_sample.py <out.rec> [target_bytes]

Two passes need the input twice, so stdin is first spooled to a zstd file
next to the output (private dir), then removed.
"""
import hashlib, json, os, struct, subprocess, sys

KINDS = {"system": 0, "user_input": 1, "thinking": 2, "tool_call": 3, "tool_result": 4, "assistant": 5, "explicit": 6}
out = sys.argv[1]
target = int(sys.argv[2]) if len(sys.argv) > 2 else 64 << 20
spool = out + ".spool.zst"
with open(spool, "wb") as f:
    subprocess.run(["zstd", "-q", "-3", "-c"], stdin=sys.stdin.buffer, stdout=f, check=True)

def lines():
    p = subprocess.Popen(["zstd", "-q", "-d", "-c", spool], stdout=subprocess.PIPE)
    for l in p.stdout:
        yield json.loads(l)
    p.wait()

size = {}
for d in lines():
    size[d["session"]] = size.get(d["session"], 0) + len(d["text"].encode())
order = sorted(size, key=lambda s: hashlib.sha256(("oojuro-owner-sample-v1:" + s).encode()).hexdigest())
keep, total = set(), 0
for s in order:
    if total >= target:
        break
    keep.add(s)
    total += size[s]
recs = [d for d in lines() if d["session"] in keep]
os.remove(spool)
recs.sort(key=lambda d: d["id"])
sid = {}
kb = {}
with open(out, "wb") as f:
    f.write(b"OJCORP01")
    for d in recs:
        t = d["text"].encode()
        k = KINDS[d["kind"]]
        f.write(struct.pack("<BIII", k, sid.setdefault(d["session"], len(sid)), d["turn"] & 0xFFFFFFFF, len(t)))
        f.write(t)
        kb[d["kind"]] = kb.get(d["kind"], 0) + len(t)
print(json.dumps({"sessions_total": len(size), "sessions_kept": len(keep), "records": len(recs), "raw": total,
                  "journal_raw": sum(size.values()), "kind_bytes": kb}))
