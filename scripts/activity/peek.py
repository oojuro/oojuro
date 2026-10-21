"""Print a window's records compactly for hand-checking (LOCAL ONLY: the
output holds transcript text; never commit or paste it anywhere).

    python3 -I peek.py WRITER_NAME START_US END_US [--session S] [--width 110] [--limit 400]

Reads through `oojuro range` (the recorder serves it; read-only).
"""
import json
import subprocess
import sys

sys.path.insert(0, __import__("os").path.dirname(__file__))
from common import Families, error_level, is_echo  # noqa: E402


def main():
    a = sys.argv[1:]
    writer, start, end = a[0], int(a[1]), int(a[2])
    opt = dict(zip(a[3::2], a[4::2]))
    width = int(opt.get("--width", 110))
    limit = int(opt.get("--limit", 400))
    session = opt.get("--session")
    cmd = ["oojuro", "--json", "range", f"@{start / 1e6:.6f}", f"@{(end - 1) / 1e6:.6f}", "--writer", writer,
           "--limit", "20000"]
    recs = json.loads(subprocess.run(cmd, capture_output=True, text=True, check=True).stdout)
    if isinstance(recs, dict):
        recs = recs.get("records") or recs.get("result") or []
    fams = Families()
    shown = 0
    for r in recs:
        fam = fams.of(r) if r["kind"] != "system" else "-"
        if session and r["session"] != session:
            continue
        if r["kind"] == "system" and not error_level(r):
            continue
        t = " ".join(r["text"].split())[:width]
        print(f"{r['ts'][11:19]} {r['session'][:6]} {r['kind'][:6]:6} {r.get('tool', '')[:14]:14} {fam:8} "
              f"e{error_level(r)} {'E' if is_echo(r) else ' '} {len(r['text']):7} | {t}")
        shown += 1
        if shown >= limit:
            print("...")
            break


if __name__ == "__main__":
    main()
