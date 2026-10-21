"""Turn local agent logs (Claude Code project JSONL, Codex rollout JSONL)
into a TraceBench TBEV0001 event file, for storage measurements on a
person's own logs. Reads the logs only; writes <out>.tbe and prints
aggregate counts (no content).

Mapping (a simplified mirror of the v1 capture parsers, without
redaction): one record per content block. Claude Code: user prompt or
tool_result blocks, assistant text / thinking / tool_use (text = the input
JSON), harness entries (isMeta, tag-wrapped prompts, system entries) as
system; noise entry types are skipped. Codex: response_item message /
reasoning summary / function and custom tool calls (text = arguments or
input) and their outputs, session base instructions, compaction summaries;
event_msg and counters are skipped. Images become a short placeholder.
Writer (agent) = Claude project folder or Codex cwd. Batch = the records of
one log line; batches are ordered by timestamp; session = source file.

Usage: python3 -I logs_to_tbe.py <out.tbe> [max_files]
"""
import glob, json, os, re, struct, sys, hashlib
from datetime import datetime

EV = struct.Struct("<QQIIIQQBBiIIqqHII")
SYSTEM, USER, THINKING, OUTPUT, TOOL_CALL, TOOL_RESULT = range(6)
NULL_EXIT = -(2 ** 31)
NOISE = {"queue-operation", "last-prompt", "atis-latch", "bridge-session", "custom-title", "agent-name",
         "pr-link", "mode", "file-history-snapshot", "file-history-delta", "cost-state", "summary",
         "progress", "tag"}
TAG = re.compile(r"^\s*<([a-zA-Z][\w-]*)>")
HUMAN_TAGS = {"command-name", "command-message", "command-args", "bash-input"}


def ts_us(s):
    if not s:
        return None
    try:
        return int(datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp() * 1_000_000)
    except Exception:
        return None


def image(media, data):
    return f"[image {media} {len(data or '')} bytes]"


def content_text(v):
    if isinstance(v, str):
        return v
    if isinstance(v, list):
        parts = []
        for b in v:
            if not isinstance(b, dict):
                parts.append(json.dumps(b))
                continue
            t = b.get("type")
            if t in ("text", "input_text", "output_text", "summary_text", "reasoning_text"):
                parts.append(b.get("text", ""))
            elif t == "image":
                src = b.get("source") or {}
                parts.append(image(src.get("media_type"), src.get("data")))
            elif t == "input_image":
                parts.append(image("url", b.get("image_url")))
            elif t == "encrypted_content":
                pass
            else:
                parts.append(json.dumps(b, ensure_ascii=False))
        return "\n".join(parts)
    if isinstance(v, dict) and isinstance(v.get("content"), (str, list)):
        return content_text(v["content"])
    if v is None:
        return ""
    return json.dumps(v, ensure_ascii=False)


def claude_records(e):
    """(kind, tool, text, status) per content block of one Claude Code entry."""
    t = e.get("type")
    out = []
    if t in NOISE or t is None:
        return out
    msg = e.get("message") or {}
    if t == "assistant":
        for b in msg.get("content") or []:
            if not isinstance(b, dict):
                continue
            bt = b.get("type")
            if bt == "text":
                out.append((OUTPUT, "", b.get("text", ""), 0))
            elif bt == "thinking":
                if b.get("thinking"):
                    out.append((THINKING, "", b["thinking"], 0))
            elif bt in ("tool_use", "server_tool_use", "mcp_tool_use"):
                out.append((TOOL_CALL, b.get("name", ""), json.dumps(b.get("input"), ensure_ascii=False, separators=(",", ":")), 0))
            elif bt and bt.endswith("_tool_result"):
                out.append((TOOL_RESULT, "", content_text(b.get("content")), 2))
            elif bt != "redacted_thinking":
                out.append((SYSTEM, "", json.dumps(b, ensure_ascii=False), 0))
    elif t == "user":
        c = msg.get("content")
        prompt = []
        if isinstance(c, str):
            prompt.append(c)
        elif isinstance(c, list):
            for b in c:
                if not isinstance(b, dict):
                    continue
                if b.get("type") == "tool_result":
                    st = 3 if b.get("is_error") else 2
                    out.append((TOOL_RESULT, "", content_text(b.get("content")), st))
                elif b.get("type") == "text":
                    prompt.append(b.get("text", ""))
                elif b.get("type") == "image":
                    src = b.get("source") or {}
                    prompt.append(image(src.get("media_type"), src.get("data")))
                else:
                    prompt.append(json.dumps(b, ensure_ascii=False))
        if prompt:
            text = "\n".join(prompt)
            m = TAG.match(text)
            harness = e.get("isMeta") or e.get("isCompactSummary") or (m and m.group(1) not in HUMAN_TAGS)
            out.append((SYSTEM if harness else USER, "", text, 0))
    elif t == "attachment":
        r = e.get("rendered")
        if isinstance(r, str) and r:
            out.append((SYSTEM, "", r, 0))
    elif t == "system":
        if isinstance(e.get("content"), str) and e["content"]:
            out.append((SYSTEM, "", e["content"], 0))
    return out


def codex_records(e, state):
    t = e.get("type")
    p = e.get("payload") if isinstance(e.get("payload"), dict) else {}
    pt = p.get("type")
    out = []
    if t == "session_meta":
        state["metas"] = state.get("metas", 0) + 1
        if state["metas"] == 1:
            state["writer"] = "codex:" + (p.get("cwd") or "unknown")
            bi = (p.get("base_instructions") or {}).get("text", "") if isinstance(p.get("base_instructions"), dict) else ""
            if bi:
                out.append((SYSTEM, "", bi, 0))
    elif t == "response_item":
        if pt == "message":
            text = content_text(p.get("content"))
            role = p.get("role")
            if role == "assistant":
                out.append((OUTPUT, "", text, 0))
            elif role == "user":
                out.append((SYSTEM if TAG.match(text) else USER, "", text, 0))
            else:
                out.append((SYSTEM, "", text, 0))
        elif pt == "reasoning":
            text = content_text(p.get("summary"))
            c = content_text(p.get("content"))
            if c:
                text = (text + "\n" + c) if text else c
            if text:
                out.append((THINKING, "", text, 0))
        elif pt in ("function_call", "custom_tool_call"):
            body = p.get("arguments" if pt == "function_call" else "input")
            body = body if isinstance(body, str) else json.dumps(body, ensure_ascii=False)
            out.append((TOOL_CALL, p.get("name", ""), body or "", 0))
        elif pt in ("function_call_output", "custom_tool_call_output"):
            out.append((TOOL_RESULT, "", content_text(p.get("output")), 0))
        elif pt in ("local_shell_call", "web_search_call", "image_generation_call", "tool_search_call"):
            out.append((TOOL_CALL, pt[:-5], json.dumps(p.get("action", p), ensure_ascii=False), 0))
        elif pt == "agent_message":
            out.append((USER, "", content_text(p.get("content")), 0))
        else:
            out.append((SYSTEM, "", json.dumps(p, ensure_ascii=False), 0))
    elif t == "compacted":
        if p.get("message"):
            out.append((SYSTEM, "", p["message"], 0))
    elif t == "world_state":
        out.append((SYSTEM, "", json.dumps(p.get("state"), ensure_ascii=False), 0))
    elif t == "realtime_item" and pt == "transcript_segment":
        out.append((USER if p.get("role") == "user" else OUTPUT, "", p.get("text", ""), 0))
    return out


def main():
    out_path = sys.argv[1]
    max_files = int(sys.argv[2]) if len(sys.argv) > 2 else 10 ** 9
    files = sorted(glob.glob(os.path.expanduser("~/.claude/projects/**/*.jsonl"), recursive=True))
    files += sorted(glob.glob(os.path.expanduser("~/.codex/sessions/**/rollout-*.jsonl"), recursive=True))
    files = files[:max_files]
    spool_path = out_path + ".spool"
    spool = open(spool_path, "wb")
    index = []  # (ts, file_no, line_no, spool_off, n, writer)
    writers = {}
    stats = {"lines": 0, "bad": 0, "records": 0, "raw": 0, "log_bytes": 0}
    kinds = [[0, 0] for _ in range(6)]
    for fno, f in enumerate(files):
        codex = os.path.basename(f).startswith("rollout-")
        if codex:
            state = {"writer": "codex:unknown"}
        else:
            parts = f.split(os.sep)
            i = len(parts) - 1 - parts[::-1].index("projects")
            state = {"writer": "claude:" + parts[i + 1]}
        last = 0
        turn = 0
        seq = 0
        with open(f, "rb") as fh:
            for lno, line in enumerate(fh):
                stats["lines"] += 1
                stats["log_bytes"] += len(line)
                try:
                    e = json.loads(line)
                except Exception:
                    stats["bad"] += 1
                    continue
                if not isinstance(e, dict):
                    continue
                t = ts_us(e.get("timestamp")) or last
                last = t
                recs = codex_records(e, state) if codex else claude_records(e)
                recs = [r for r in recs if r[2] is not None]
                if not recs:
                    continue
                w = writers.setdefault(state["writer"], len(writers))
                buf = bytearray()
                for kind, tool, text, st in recs:
                    if kind == USER:
                        turn += 1
                    tb = tool.encode("utf-8", "replace")[:65535]
                    xb = text.encode("utf-8", "replace")
                    buf += EV.pack(0, fno + 1, w, turn, seq, 0, 0, kind, st, NULL_EXIT, 0, (len(xb) + 3) // 4,
                                   t, 0, len(tb), 0, len(xb))
                    buf += tb + xb
                    seq += 1
                    stats["records"] += 1
                    stats["raw"] += len(tb) + len(xb)
                    kinds[kind][0] += 1
                    kinds[kind][1] += len(tb) + len(xb)
                index.append((t, fno, lno, spool.tell(), len(recs), w))
                spool.write(buf)
    spool.close()
    index.sort()
    sess = {}
    nid = 1
    with open(spool_path, "rb") as sp, open(out_path, "wb") as out:
        out.write(b"TBEV0001")
        for t, fno, lno, off, n, w in index:
            sp.seek(off)
            out.write(struct.pack("<I", n))
            for _ in range(n):
                head = bytearray(sp.read(EV.size))
                f = list(EV.unpack(head))
                f[0] = nid
                nid += 1
                f[1] = sess.setdefault(f[1], len(sess) + 1)
                tl, al, xl = f[14], f[15], f[16]
                out.write(EV.pack(*f))
                out.write(sp.read(tl + al + xl))
    os.remove(spool_path)
    names = ["system", "user", "thinking", "output", "tool_call", "tool_result"]
    print(json.dumps({"files": len(files), "writers": len(writers), "sessions": len(sess), **stats,
                      "kinds": {names[i]: {"records": k[0], "raw_mb": round(k[1] / 1e6, 1)} for i, k in enumerate(kinds)}}))


main()
