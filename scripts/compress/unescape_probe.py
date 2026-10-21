"""E9 probe: whole-sample zstd 19 of a corpus with nested tool-call JSON unescaped (canonical only).
Usage: .scratch/venv/bin/python -I scripts/compress/unescape_probe.py <corpus.rec>"""
import json, struct, sys, zstandard
path = sys.argv[1]
f = open(path, 'rb'); assert f.read(8) == b'OJCORP01'
raw, tx, ok, tried = [], [], 0, 0
def canon(o):
    return json.dumps(o, ensure_ascii=False, separators=(',', ':'))
while True:
    h = f.read(13)
    if len(h) < 13: break
    k, s, t, l = struct.unpack('<BIII', h); b = f.read(l)
    raw.append(b)
    if k != 3:
        tx.append(b); continue
    tried += 1
    try:
        o = json.loads(b)
        if canon(o).encode() != b: raise ValueError
        args = o['function']['arguments']
        inner = json.loads(args)
        # inner must re-serialize exactly with Python's default separators
        if json.dumps(inner, ensure_ascii=False).encode() != args.encode(): raise ValueError
        # transformed: header line + each inner field raw, separated by sentinels
        parts = [b'\x01' + o['id'].encode() + b'\x02' + o['function']['name'].encode()]
        for key, v in inner.items():
            parts.append(b'\x03' + key.encode() + b'\x04' + (v.encode() if isinstance(v, str) else canon(v).encode()))
        tx.append(b''.join(parts)); ok += 1
    except Exception:
        tx.append(b)
c = zstandard.ZstdCompressor(level=19)
r = b''.join(raw); t = b''.join(tx)
zr, zt = len(c.compress(r)), len(c.compress(t))
print(json.dumps({"corpus": path, "tool_calls": tried, "transformed": ok, "raw": len(r), "zstd19_raw": zr, "zstd19_transformed": zt, "gain": 1 - zt / zr}))
