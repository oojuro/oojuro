"""Normalize public agent corpora into one append-order record stream per corpus.

Output (never committed): <out>/<name>.rec, a flat binary stream
    magic b"OJCORP01"
    repeated: kind u8 | session u32 | turn u32 | length u32 | utf-8 bytes
plus <out>/<name>.manifest.json (counts, bytes per kind, sha256 of the .rec
file and of every source file). Only manifests are committed.

Kinds model what an agent appends: the turn input (user), thinking text, the tool
call, the tool result, the final answer. "system" is the per-session preamble
(system prompt and tool schemas), "doc" is a BEIR control document.

Usage: python -I build_corpus.py <raw_dir> <out_dir> <name> [max_raw_bytes]
"""
import hashlib, json, os, re, struct, sys

KINDS = {"system": 0, "user": 1, "thinking": 2, "tool_call": 3, "tool_result": 4, "answer": 5, "doc": 6}
HERE = os.path.dirname(os.path.abspath(__file__))


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


class Writer:
    def __init__(self, path, budget):
        self.f = open(path, "wb")
        self.f.write(b"OJCORP01")
        self.budget, self.raw, self.records, self.sessions = budget, 0, 0, 0
        self.by_kind = {k: [0, 0] for k in KINDS}
        self.session, self.turn = -1, 0

    def full(self):
        return self.budget and self.raw >= self.budget

    def begin(self):
        self.sessions += 1
        self.session += 1
        self.turn = 0

    def next_turn(self):
        self.turn += 1

    def put(self, kind, text):
        if text is None or text == "":
            return
        b = text.encode("utf-8")
        self.f.write(struct.pack("<BIII", KINDS[kind], self.session, self.turn, len(b)))
        self.f.write(b)
        self.raw += len(b)
        self.records += 1
        self.by_kind[kind][0] += 1
        self.by_kind[kind][1] += len(b)


FENCE = re.compile(r"```[^\n]*\n(.*?)```\s*$", re.S)


def swe_agent(raw, w):
    """nebius/SWE-agent-trajectories: ai = thought prose + fenced command; user = observation."""
    import pyarrow.parquet as pq
    for name in sorted(os.listdir(raw)):
        pf = pq.ParquetFile(os.path.join(raw, name))
        for batch in pf.iter_batches(batch_size=64, columns=["trajectory"]):
            for traj in batch.column(0).to_pylist():
                if w.full():
                    return
                w.begin()
                seen_user = False
                for m in traj:
                    role, text = m["role"], m["text"] or ""
                    if role == "system":
                        w.put("system", m["system_prompt"] or text)
                    elif role == "user":
                        w.put("tool_result" if seen_user else "user", text)
                        seen_user = True
                    else:
                        w.next_turn()
                        mt = FENCE.search(text)
                        if mt:
                            w.put("thinking", text[: mt.start()])
                            w.put("tool_call", text[mt.start():])
                        else:
                            w.put("thinking", text)


FUNC = re.compile(r"<function=", re.S)


def openhands_sft(raw, w):
    """SWE-Gym/OpenHands-SFT-Trajectories: assistant = prose + <function=...> call."""
    import pyarrow.parquet as pq
    for name in sorted(os.listdir(raw)):
        pf = pq.ParquetFile(os.path.join(raw, name))
        for batch in pf.iter_batches(batch_size=64, columns=["messages"]):
            for msgs in batch.column(0).to_pylist():
                if w.full():
                    return
                w.begin()
                seen_user = False
                for m in msgs:
                    role, text = m["role"], m["content"] or ""
                    if role == "system":
                        w.put("system", text)
                    elif role in ("user", "tool"):
                        w.put("tool_result" if seen_user else "user", text)
                        seen_user = True
                    else:
                        w.next_turn()
                        mt = FUNC.search(text)
                        if mt:
                            w.put("thinking", text[: mt.start()])
                            w.put("tool_call", text[mt.start():])
                        else:
                            w.put("answer", text)


def compact(v):
    return json.dumps(v, ensure_ascii=False, separators=(",", ":"))


def drop_none(v):
    if isinstance(v, dict):
        return {k: drop_none(x) for k, x in v.items() if x is not None}
    if isinstance(v, list):
        return [drop_none(x) for x in v]
    return v


def openhands_json(raw, w):
    """nebius/SWE-rebench-openhands-trajectories, first row group: OpenAI-style JSON tool calls."""
    sys.path.insert(0, HERE)
    from httpfile import HttpFile
    import pyarrow.parquet as pq
    url = open(os.path.join(raw, "URL")).read().strip()
    pf = pq.ParquetFile(HttpFile(url))
    for batch in pf.iter_batches(batch_size=16, row_groups=[0], columns=["trajectory", "tools"]):
        trajs, tools = batch.column(0).to_pylist(), batch.column(1).to_pylist()
        for traj, tl in zip(trajs, tools):
            if w.full():
                return
            w.begin()
            w.put("system", compact(drop_none(tl)))
            seen_user = False
            for m in traj:
                role, text = m["role"], m["content"] or ""
                if role == "system":
                    w.put("system", text)
                elif role == "user":
                    w.put("tool_result" if seen_user else "user", text)
                    seen_user = True
                elif role == "tool":
                    w.put("tool_result", text)
                else:
                    w.next_turn()
                    calls = m.get("tool_calls") or []
                    w.put("thinking" if calls else "answer", text)
                    for c in calls:
                        w.put("tool_call", compact(drop_none(c)))


def hermes(raw, w):
    """NousResearch/hermes-function-calling-v1: system(tools) / human / gpt(<tool_call>) / tool."""
    for name in sorted(os.listdir(raw)):
        if not name.endswith(".json"):
            continue
        for conv in json.load(open(os.path.join(raw, name))):
            if w.full():
                return
            w.begin()
            for m in conv["conversations"]:
                frm, text = m["from"], m["value"]
                if frm == "system":
                    w.put("system", text)
                elif frm == "human":
                    w.put("user", text)
                elif frm == "tool":
                    w.put("tool_result", text)
                else:
                    w.next_turn()
                    w.put("tool_call" if "<tool_call>" in text else "answer", text)


def openthoughts(raw, w):
    """open-thoughts/OpenThoughts-114k: long reasoning traces; thought block = thinking."""
    import pyarrow.parquet as pq
    for name in sorted(os.listdir(raw)):
        pf = pq.ParquetFile(os.path.join(raw, name))
        for batch in pf.iter_batches(batch_size=64, columns=["system", "conversations"]):
            systems, convs = batch.column(0).to_pylist(), batch.column(1).to_pylist()
            for system, conv in zip(systems, convs):
                if w.full():
                    return
                w.begin()
                w.put("system", system)
                for m in conv:
                    text = m["value"]
                    if m["from"] == "user":
                        w.put("user", text)
                        continue
                    w.next_turn()
                    end = text.find("<|end_of_thought|>")
                    if end >= 0:
                        end += len("<|end_of_thought|>")
                        w.put("thinking", text[:end])
                        w.put("answer", text[end:])
                    else:
                        w.put("answer", text)


def beir(raw, w):
    import pyarrow.parquet as pq
    for name in sorted(os.listdir(raw)):
        pf = pq.ParquetFile(os.path.join(raw, name))
        for batch in pf.iter_batches(batch_size=1024, columns=["title", "text"]):
            for title, text in zip(batch.column(0).to_pylist(), batch.column(1).to_pylist()):
                if w.full():
                    return
                w.begin()
                w.put("doc", (title + "\n" + text) if title else text)


SOURCES = {"swe": swe_agent, "ohsft": openhands_sft, "ohjson": openhands_json, "hermes": hermes,
           "othink": openthoughts, "fiqa": beir, "nfcorpus": beir}


def main():
    raw, out, name = sys.argv[1], sys.argv[2], sys.argv[3]
    budget = int(sys.argv[4]) if len(sys.argv) > 4 else 0
    os.makedirs(out, exist_ok=True)
    path = os.path.join(out, name + ".rec")
    w = Writer(path, budget)
    SOURCES[name](raw, w)
    w.f.close()
    manifest = {
        "name": name, "records": w.records, "sessions": w.sessions, "raw_bytes": w.raw,
        "budget_bytes": budget,
        "by_kind": {k: {"records": v[0], "bytes": v[1]} for k, v in w.by_kind.items() if v[0]},
        "rec_sha256": sha256(path),
        "sources": {f: sha256(os.path.join(raw, f)) for f in sorted(os.listdir(raw))
                    if os.path.isfile(os.path.join(raw, f)) and f != "URL"},
    }
    if os.path.exists(os.path.join(raw, "URL")):
        manifest["source_url"] = open(os.path.join(raw, "URL")).read().strip()
    json.dump(manifest, open(os.path.join(out, name + ".manifest.json"), "w"), indent=1)
    print(json.dumps({k: manifest[k] for k in ("name", "records", "sessions", "raw_bytes")}))


if __name__ == "__main__":
    main()
