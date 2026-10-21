"""Build the TraceBench base event stream from public agent trajectories.

Output (never committed; only the manifest is): <out>/<name>.tbe and
<out>/<name>.manifest.json.

File format TBEV0001 (little-endian):
    magic b"TBEV0001"
    repeated batch: n_events u32, then n_events events:
        id u64 | session u64 | agent u32 | turn u32 | seq u32 | parent u64 |
        revises u64 | kind u8 | status u8 | exit_code i32 | tokens_in u32 |
        tokens_out u32 | ts_us i64 | duration_us i64 |
        tool_len u16 | args_len u32 | text_len u32 | tool | args | text
Batches are in canonical append order (append time, then agent). Event ids
are dense (1..N) in that order. A batch is everything one agent appends at
the end of one turn: one durable commit in every arm.

Kinds: 0 system, 1 user, 2 thinking, 3 output, 4 tool_call, 5 tool_result.
Status: 0 none, 1 pending, 2 ok, 3 error. exit_code = -2^31 means null.
A status revision is a tool_call event with revises = the call's id, empty
text and args, the final status, exit code and duration ("the tool call
completed"). It rides in the batch that carries the tool's result, or in a
revision-only batch when the source has no result. Arms may apply it as an
UPDATE or as an appended revision; every arm must answer the latest status.

Derived (not in the sources), deterministic, documented in TRACEBENCH.md:
timestamps (latency model in `stamp`), agents and interleaving, token counts
(ceil(utf8 bytes / 4); tokens_in = context so far), SWE-agent exit codes
(error heuristics on the observation). OpenHands exit codes are real.

Usage: python -I build_events.py <out_dir> <name> <target_original_events>
"""
import hashlib, json, os, re, struct, sys

DATA = os.path.expanduser(os.environ.get("OOJURO_DATA", "~/.cache/oojuro-data"))  # python3 -I scripts/bench/fetch.py ...
SWE = [f"{DATA}/swe-agent/train-0000{i}-of-00012.parquet" for i in (0, 1)]
OHSFT = f"{DATA}/openhands-sft/train.parquet"
HERMES = f"{DATA}/hermes-fc/func-calling.json"
OTHINK = f"{DATA}/openthoughts/train-00000-of-00006.parquet"

SYSTEM, USER, THINKING, OUTPUT, TOOL_CALL, TOOL_RESULT = range(6)
KIND_NAMES = ["system", "user", "thinking", "output", "tool_call", "tool_result"]
NONE, PENDING, OK, ERROR = range(4)
NULL_EXIT = -(2**31)
EPOCH_US = 1_788_220_800_000_000  # 2026-09-01T00:00:00Z
WINDOW_US = 30 * 86_400 * 1_000_000  # each agent's sessions fill a 30-day window
AGENTS = 16
OTHINK_SESSIONS = 4000
MODEL = (THINKING, OUTPUT, TOOL_CALL)

EV = struct.Struct("<QQIIIQQBBiIIqqHII")
SP = struct.Struct("<BBiHII")


def h64(*parts):
    d = hashlib.blake2b("|".join(map(str, parts)).encode(), digest_size=8).digest()
    return int.from_bytes(d, "little")


def unit(*parts):
    return h64(*parts) / 2**64


def toks(n):
    return (n + 3) // 4


def enc(s):
    return (s or "").encode("utf-8", "replace")


def ev(kind, text, tool="", args=None, status=NONE, exit_code=NULL_EXIT):
    a = json.dumps(args, ensure_ascii=False, separators=(",", ":")).encode() if args is not None else b""
    return [kind, status, exit_code, enc(tool), a, enc(text)]


ERR_RE = re.compile(r"Traceback \(most recent call last\)|\bError:|error:|command not found|No such file|"
                    r"SyntaxError|is not defined|cannot find|Usage: ", re.I)
FENCE = re.compile(r"```[^\n]*\n(.*?)```\s*$", re.S)


def new_model_turn(turns):
    if len(turns) == 1 or turns[-1][-1][0] not in (TOOL_RESULT, USER):
        turns.append([])


def swe_sessions():
    """nebius/SWE-agent-trajectories: ai = thought + fenced command; user = observation."""
    import pyarrow.parquet as pq
    for path in SWE:
        pf = pq.ParquetFile(path)
        for batch in pf.iter_batches(batch_size=32, columns=["trajectory", "generated_patch", "exit_status"]):
            for traj, patch, exit_status in zip(*(batch.column(i).to_pylist() for i in range(3))):
                turns, cur, seen_user = [[]], None, False
                for m in traj:
                    role, text = m["role"], m["text"] or ""
                    if role == "system":
                        turns[0].append(ev(SYSTEM, m["system_prompt"] or text))
                    elif role == "user" and not seen_user:
                        turns[0].append(ev(USER, text))
                        seen_user = True
                    elif role == "user" and cur is None:
                        turns.append([ev(USER, text)])
                    elif role == "user":
                        bad = ERR_RE.search(text[:4096]) is not None
                        turns.append([ev(TOOL_RESULT, text, cur or "", status=ERROR if bad else OK,
                                         exit_code=1 if bad else 0)])
                    else:
                        new_model_turn(turns)
                        mt = FENCE.search(text)
                        if mt:
                            first = mt.group(1).strip().split("\n", 1)[0].split()
                            cur = first[0] if first else "?"
                            turns[-1].append(ev(THINKING, text[: mt.start()]))
                            turns[-1].append(ev(TOOL_CALL, text[mt.start():], cur,
                                                {"argv": " ".join(first[1:])[:256]}, PENDING))
                        else:
                            cur = None
                            turns[-1].append(ev(THINKING, text))
                if patch:
                    turns.append([ev(OUTPUT, patch, args={"exit_status": exit_status})])
                yield "swe", turns


FUNC = re.compile(r"<function=([^>\n]*)>")
PARAM = re.compile(r"<parameter=([^>\n]*)>(.*?)</parameter>", re.S)
EXIT_RE = re.compile(r"\[Command finished with exit code (-?\d+)\]")


def ohsft_sessions():
    """SWE-Gym/OpenHands-SFT-Trajectories: <function=NAME><parameter=k>v</parameter>; real exit codes."""
    import pyarrow.parquet as pq
    pf = pq.ParquetFile(OHSFT)
    for batch in pf.iter_batches(batch_size=32, columns=["messages"]):
        for msgs in batch.column(0).to_pylist():
            turns, cur, seen_user = [[]], None, False
            for m in msgs:
                role, text = m["role"], m["content"] or ""
                if role == "system":
                    turns[0].append(ev(SYSTEM, text))
                elif role in ("user", "tool") and not seen_user:
                    turns[0].append(ev(USER, text))
                    seen_user = True
                elif role in ("user", "tool") and cur is None:
                    turns.append([ev(USER, text)])
                elif role in ("user", "tool"):
                    mx = EXIT_RE.search(text)
                    code = int(mx.group(1)) if mx else (1 if "ERROR" in text[:300] else 0)
                    turns.append([ev(TOOL_RESULT, text, cur or "", status=OK if code == 0 else ERROR,
                                     exit_code=code)])
                else:
                    new_model_turn(turns)
                    mt = FUNC.search(text)
                    if mt:
                        cur = mt.group(1).strip()
                        params = {k: v[:256] for k, v in PARAM.findall(text[mt.start():])}
                        if text[: mt.start()].strip():
                            turns[-1].append(ev(THINKING, text[: mt.start()]))
                        turns[-1].append(ev(TOOL_CALL, text[mt.start():], cur, params, PENDING))
                    else:
                        cur = None
                        turns[-1].append(ev(OUTPUT, text))
            yield "ohsft", turns


TOOL_CALL_RE = re.compile(r"<tool_call>\s*(\{.*?\})\s*</tool_call>", re.S)


def hermes_sessions():
    """NousResearch/hermes-function-calling-v1 func-calling.json: JSON tool calls, one result per call turn."""
    for conv in json.load(open(HERMES)):
        turns, cur, seen = [[]], None, False
        for m in conv["conversations"]:
            frm, text = m["from"], m["value"]
            if frm == "system":
                turns[0].append(ev(SYSTEM, text))
            elif frm == "human":
                if seen:
                    turns.append([])
                turns[-1].append(ev(USER, text))
                seen = True
            elif frm == "tool":
                turns.append([ev(TOOL_RESULT, text, cur or "", status=OK, exit_code=0)])
            else:
                if turns[-1] and turns[-1][-1][0] not in (TOOL_RESULT, USER):
                    turns.append([])
                calls = []
                for c in TOOL_CALL_RE.findall(text):
                    try:
                        j = json.loads(c)
                        calls.append({"name": j.get("name"), "arguments": j.get("arguments")})
                    except ValueError:
                        calls.append({"name": "?", "arguments": None})
                if calls or "<tool_call>" in text:
                    cur = (calls[0]["name"] if calls else "?") or "?"
                    turns[-1].append(ev(TOOL_CALL, text, cur, calls, PENDING))
                else:
                    cur = None
                    turns[-1].append(ev(OUTPUT, text))
        yield "hermes", turns


def othink_sessions():
    """open-thoughts/OpenThoughts-114k: long reasoning; thought block = thinking, solution = output."""
    import pyarrow.parquet as pq
    pf = pq.ParquetFile(OTHINK)
    n = 0
    for batch in pf.iter_batches(batch_size=32, columns=["system", "conversations"]):
        for system, conv in zip(batch.column(0).to_pylist(), batch.column(1).to_pylist()):
            if n >= OTHINK_SESSIONS:
                return
            n += 1
            turns = [[ev(SYSTEM, system)]]
            for m in conv:
                text = m["value"]
                if m["from"] == "user":
                    turns[-1].append(ev(USER, text))
                    continue
                turns.append([])
                end = text.find("<|end_of_thought|>")
                if end >= 0:
                    end += len("<|end_of_thought|>")
                    turns[-1].append(ev(THINKING, text[:end]))
                    turns[-1].append(ev(OUTPUT, text[end:]))
                else:
                    turns[-1].append(ev(OUTPUT, text))
            yield "othink", turns


def stamp(i, turns):
    """Relative event times (us) for one session.

    Model step: 400 ms + 15 ms per output token of the step. Tool run: 50 ms to
    25.6 s, log-uniform, hashed per (session, turn). Session start events are 0.
    """
    t, call_ts, out = 0, 0, []
    for ti, turn in enumerate(turns):
        st = []
        for ei, e in enumerate(turn):
            kind = e[0]
            if kind == TOOL_RESULT:
                t = call_ts + int(50_000 * 2 ** (9 * unit("tool", i, ti)))
            elif kind in MODEL and (ei == 0 or turn[ei - 1][0] not in MODEL):
                step = sum(toks(len(x[5])) for x in turn[ei:] if x[0] in MODEL)
                t += 400_000 + 15_000 * step
            st.append(t)
            if kind == TOOL_CALL:
                call_ts = t
        out.append(st)
    return out, t


def main():
    out, name, target = sys.argv[1], sys.argv[2], int(sys.argv[3])
    os.makedirs(out, exist_ok=True)
    spool_path = os.path.join(out, name + ".spool")
    spool = open(spool_path, "wb")
    # Pass 1: parse, spool each session's events, keep kinds and relative times.
    sessions = []  # (source, spool offset, kinds per turn, stamps per turn, duration)
    stats, total, nul = {}, 0, 0

    def take(src, turns):
        nonlocal total, nul
        turns = [t for t in turns if t]
        if not turns:
            return
        i = len(sessions)
        blob = []
        for t in turns:
            for e in t:
                if b"\x00" in e[5]:
                    nul += 1
                    e[5] = e[5].replace(b"\x00", "\N{REPLACEMENT CHARACTER}".encode())
                blob.append(SP.pack(e[0], e[1], e[2], len(e[3]), len(e[4]), len(e[5])) + e[3] + e[4] + e[5])
        data = b"".join(blob)
        off = spool.tell()
        spool.write(struct.pack("<I", len(data)) + data)
        stamps, dur = stamp(i, turns)
        sessions.append((src, off, [[e[0] for e in t] for t in turns], stamps, dur))
        n = sum(len(t) for t in turns)
        total += n
        s = stats.setdefault(src, {"sessions": 0, "events": 0, "bytes": 0})
        s["sessions"] += 1
        s["events"] += n
        s["bytes"] += sum(len(e[3]) + len(e[4]) + len(e[5]) for t in turns for e in t)

    for gen in (ohsft_sessions(), hermes_sessions(), othink_sessions(), swe_sessions()):
        for src, turns in gen:
            if total >= target:
                break
            take(src, turns)
    spool.close()
    print(f"parsed {len(sessions)} sessions, {total} original events", file=sys.stderr)

    # Sessions are shuffled (hash order) and dealt round-robin to agents. Each agent
    # runs its sessions back to back with idle gaps sized to span the window.
    order = sorted(range(len(sessions)), key=lambda i: h64("order", i))
    by_agent = [order[a::AGENTS] for a in range(AGENTS)]
    start = {}
    plan = []  # (append_ts, agent, session, turn, revision_only)
    for a, sess in enumerate(by_agent):
        busy = sum(sessions[i][4] for i in sess)
        gap = max(1_000_000, (WINDOW_US - busy) // max(1, len(sess)))
        t0 = EPOCH_US + int(gap * unit("agent", a))
        for i in sess:
            start[i] = (a, t0)
            kinds, stamps = sessions[i][2], sessions[i][3]
            for ti, st in enumerate(stamps):
                plan.append((t0 + max(st), a, i, ti, False))
                if TOOL_CALL in kinds[ti] and not (ti + 1 < len(kinds) and kinds[ti + 1][0] == TOOL_RESULT):
                    plan.append((t0 + max(st) + 1, a, i, ti, True))
            t0 += sessions[i][4] + int(gap * (0.5 + unit("gap", i)))
    plan.sort()

    def carries_revision(kinds, ti):
        return ti > 0 and kinds[ti][0] == TOOL_RESULT and TOOL_CALL in kinds[ti - 1]

    first_id, next_id, revisions = {}, 1, 0
    for (_, _, i, ti, tail) in plan:
        kinds = sessions[i][2]
        first_id[(i, ti, tail)] = next_id
        if tail:
            next_id += 1
            revisions += 1
        else:
            extra = 1 if carries_revision(kinds, ti) else 0
            next_id += len(kinds[ti]) + extra
            revisions += extra

    def gid(i, ti, ei):
        # the revision row sits right after the turn's leading tool_result
        return first_id[(i, ti, False)] + ei + (1 if ei >= 1 and carries_revision(sessions[i][2], ti) else 0)

    def call_at(kinds_t):
        return max(k for k, x in enumerate(kinds_t) if x == TOOL_CALL)

    sp = open(spool_path, "rb")

    def load(i):
        sp.seek(sessions[i][1])
        (n,) = struct.unpack("<I", sp.read(4))
        data, p, evs = sp.read(n), 0, []
        while p < len(data):
            kind, status, ex, tl, al, xl = SP.unpack_from(data, p)
            p += SP.size
            evs.append((kind, status, ex, data[p:p + tl], data[p + tl:p + tl + al], data[p + tl + al:p + tl + al + xl]))
            p += tl + al + xl
        turns, k = [], 0
        for t in sessions[i][2]:
            turns.append(evs[k:k + len(t)])
            k += len(t)
        return turns

    path = os.path.join(out, name + ".tbe")
    f = open(path, "wb", buffering=1 << 22)
    f.write(b"TBEV0001")
    h = hashlib.sha256(b"TBEV0001")
    by_kind, raw_bytes, cache, ctx_tokens, seqs = {}, 0, {}, {}, {}
    for (_, a, i, ti, tail) in plan:
        if i not in cache:
            if len(cache) > 2048:
                cache.clear()
            cache[i] = load(i)
        turns, kinds, stamps = cache[i], sessions[i][2], sessions[i][3]
        t0 = start[i][1]
        rows = []
        if tail:
            ci = call_at(kinds[ti])
            c = gid(i, ti, ci)
            rows.append((first_id[(i, ti, True)], i + 1, a, ti, 0, c, c, TOOL_CALL, OK, 0,
                         0, 0, t0 + stamps[ti][ci] + 1, 1, turns[ti][ci][3], b"", b""))
        else:
            nid = first_id[(i, ti, False)]
            for ei, (kind, status, ex, tool, args, text) in enumerate(turns[ti]):
                ts = t0 + stamps[ti][ei]
                if kind == TOOL_RESULT and carries_revision(kinds, ti) and ei == 0:
                    parent = gid(i, ti - 1, call_at(kinds[ti - 1]))
                elif ei > 0:
                    parent = gid(i, ti, ei - 1)
                elif ti > 0:
                    parent = gid(i, ti - 1, len(kinds[ti - 1]) - 1)
                else:
                    parent = 0
                tin = tout = 0
                if kind in MODEL:
                    tout = toks(len(text))
                    if ei == 0 or kinds[ti][ei - 1] not in MODEL:
                        tin = ctx_tokens.get(i, 0)
                rows.append((nid, i + 1, a, ti, 0, parent, 0, kind, status, ex, tin, tout, ts, 0,
                             tool, args, text))
                nid += 1
                ctx_tokens[i] = ctx_tokens.get(i, 0) + toks(len(text) + len(args))
                if ei == 0 and carries_revision(kinds, ti):
                    ci = call_at(kinds[ti - 1])
                    c = gid(i, ti - 1, ci)
                    rows.append((nid, i + 1, a, ti, 0, c, c, TOOL_CALL, status, ex, 0, 0,
                                 ts, ts - (t0 + stamps[ti - 1][ci]), turns[ti - 1][ci][3], b"", b""))
                    nid += 1
        # seq: position in the session's append order, revisions included
        n0 = seqs.get(i, 0)
        rows = [r[:4] + (n0 + k,) + r[5:] for k, r in enumerate(rows)]
        seqs[i] = n0 + len(rows)
        head = struct.pack("<I", len(rows))
        f.write(head)
        h.update(head)
        for e in rows:
            rec = EV.pack(*e[:14], len(e[14]), len(e[15]), len(e[16])) + e[14] + e[15] + e[16]
            f.write(rec)
            h.update(rec)
            kname = "status_revision" if e[6] else KIND_NAMES[e[7]]
            s = by_kind.setdefault(kname, [0, 0])
            nb = len(e[14]) + len(e[15]) + len(e[16])
            s[0] += 1
            s[1] += nb
            raw_bytes += nb
    f.close()
    sp.close()
    os.remove(spool_path)
    manifest = {
        "name": name, "format": "TBEV0001", "events": next_id - 1, "original_events": total,
        "status_revisions": revisions, "batches": len(plan), "sessions": len(sessions), "agents": AGENTS,
        "raw_bytes": raw_bytes, "window_days": 30, "epoch_us": EPOCH_US, "nul_replaced_events": nul,
        "by_source": stats, "by_kind": {k: {"events": v[0], "bytes": v[1]} for k, v in sorted(by_kind.items())},
        "sha256": h.hexdigest(),
        "sources": [os.path.basename(p) for p in SWE + [OHSFT, HERMES, OTHINK]],
    }
    json.dump(manifest, open(os.path.join(out, name + ".manifest.json"), "w"), indent=1)
    print(json.dumps({k: manifest[k] for k in ("events", "original_events", "batches", "sessions", "raw_bytes")}))


if __name__ == "__main__":
    main()
