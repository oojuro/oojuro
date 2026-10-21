"""Assemble the Section D frontier curves from measured results (JSON to stdout).

1. ratio vs decode speed: random-access block codecs (compress track's
   blocks.jsonl, swe 64 MiB sample, read-only input) + this track's 64 KiB
   frame probe on tier-1 text + whole-stream floors (no random access).
2. throughput vs durability window: fsync-probe lines (one sync per commit
   with W writers on per-writer files or one shared log; and open-loop
   writers acknowledged before a sync every T ms).
Usage: python3 frontier.py <compress blocks.jsonl> <bounds.jsonl> <floors.jsonl> <fsync.jsonl>
"""
import json, sys


def lines(p):
    try:
        return [json.loads(l) for l in open(p) if l.strip()]
    except FileNotFoundError:
        return []


blocks, bounds, floors, fsync = (lines(p) for p in sys.argv[1:5])
ratio_speed = []
for b in blocks:
    if b.get("corpus") == "swe" and b.get("mode") == "blocks":
        ratio_speed.append({"source": "compress-track blocks (swe 64 MiB)", "codec": b["codec"],
                            "block_bytes": b["block_bytes"], "dict_bytes": b.get("dict_bytes", 0),
                            "ratio": b["ratio"], "decode_mb_s": b["d_mbps"], "fetch_us_p50": b.get("fetch_us_p50")})
for b in bounds:
    if b.get("probe") == "zstd_frame_decode":
        ratio_speed.append({"source": "tracedb 64 KiB frames (tier-1 text)", "codec": f"zstd:{b['level']}",
                            "block_bytes": 65536, "dict_bytes": 524288 if b["dict"] else 0,
                            "ratio": b["ratio"], "decode_mb_s": b["mb_per_s"]})
memcpy = next((b["gb_per_s"] for b in bounds if b.get("probe") == "memcpy_1thread"), None)
whole = [{"codec": f["codec"], "order": f["order"], "ratio": f["ratio"]} for f in floors]
durability = []
for f in fsync:
    if f["mode"] in ("files", "shared"):
        durability.append({"mode": f["mode"], "writers": f["writers"], "window_ms": 0, "commits_per_s": f["commits_per_s"],
                           "p50_ms": f["p50_ms"], "p99_ms": f["p99_ms"], "at_risk": "none (durable on ack)"})
    elif f["mode"] == "async":
        durability.append({"mode": "async", "writers": f["writers"], "window_ms": f["window_ms"],
                           "commits_per_s": f["commits_per_s"], "p50_ms": f["p50_ms"], "p99_ms": f["p99_ms"],
                           "at_risk": f"≤ {f['window_ms']} ms of acknowledged commits"})
single = [f for f in fsync if f["mode"] == "single"]
print(json.dumps({"memcpy_gb_s": memcpy, "ratio_vs_decode": ratio_speed, "whole_stream_floors": whole,
                  "durability": durability, "single_writer_fsync": single}, indent=1))
