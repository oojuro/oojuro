#!/usr/bin/env python3
"""Turn fetched datasets into the harness's labelled JSONL (deterministic).

Usage: $OOJURO_DATA/.venv/bin/python -I scripts/bench/prepare.py turnloop swe-agent|hermes-fc
       $OOJURO_DATA/.venv/bin/python -I scripts/bench/prepare.py memqa longmemeval
       ... prepare.py --check   (verify prepared files against bench/prepared.json)

Outputs go to $OOJURO_DATA/prepared/{turnloop,memqa}/<set>.jsonl and
their sha256 is recorded in bench/prepared.json (commit it). Labelling rules are
versioned (LABELS) and documented in docs/BENCH_HARNESS.md; changing a rule
means bumping the version and re-preparing.

Turn-loop trajectory line:
  {"traj","group","split","init":[Rec],"steps":[{"dt","query","source","relevant":{id:gain},"records":[Rec]}]}
  Rec = {"id","kind":task|tools|thinking|call|result|answer,"dt","text","meta":{...}}
Memory-QA question line:
  {"qid","type","split","question","at","records":[{"id","session","valid_at","text","meta"}],
   "relevant":{id:1},"relevant_sessions":[sid]}
"""

import argparse
import hashlib
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DATA = Path(os.environ.get("OOJURO_DATA", "~/.cache/oojuro-data")).expanduser()
MANIFEST = REPO / "bench" / "prepared.json"
LABELS = "turnloop-labels-v1"
QUERY_CAP = 4096          # bytes of turn input used as the query (head + tail)
RECORD_CAP = 128 * 1024   # longer records are truncated with a visible marker
MAX_REFERENCED = 3        # a token found in more earlier results is not distinctive
READ_COMMANDS = {"open", "cat", "search_file", "search_dir", "find_file", "grep", "ls", "find", "head", "tail"}
PATH_COMMANDS = {"open", "cat"}
TOKEN = re.compile(r"[A-Za-z_][A-Za-z0-9_]*(?:[./][A-Za-z_][A-Za-z0-9_]*)+|[A-Za-z_][A-Za-z0-9_]{5,}")
FENCE = re.compile(r"```[a-zA-Z]*\n(.*?)```", re.S)


def splits(name):
    body = json.loads((REPO / "bench" / "splits" / f"{name}.json").read_text())
    return {i: "dev" for i in body["dev"]} | {i: "heldout" for i in body["heldout"]}


def cap_bytes(text, cap, marker):
    raw = text.encode()
    if len(raw) <= cap:
        return text
    if marker:
        return raw[:cap].decode(errors="ignore") + f"\n[oojuro-bench: truncated {len(raw) - cap} bytes]"
    half = cap // 2
    return raw[:half].decode(errors="ignore") + "\n…\n" + raw[-half:].decode(errors="ignore")


def distinctive(text):
    """Identifier-like tokens: dotted/slashed names, or >=6 chars with _ digit or inner capital."""
    out = set()
    for t in TOKEN.findall(text):
        if "." in t or "/" in t or "_" in t or any(c.isdigit() for c in t) or any(c.isupper() for c in t[1:]):
            out.add(t)
    return out


def normalize(action):
    return " ".join(action.split())


def command(action):
    parts = action.split()
    return (parts[0], parts[1:]) if parts else ("", [])


def label_steps(steps, results, task_tokens, stop_tokens):
    """Fill steps[i]['relevant'] from what the agent does at step i (see docs).

    R1a gain 2: step i runs a read command identical to an earlier step's call;
                that earlier result is relevant.
    R1b gain 1: step i opens/cats the same path as an earlier open/cat.
    R2  gain 1: step i's thinking+call contains a distinctive token that is not
                in the task, the system prompt or this step's own input, and
                occurs in 1..MAX_REFERENCED earlier results; those are relevant.
    The query's own source record is never relevant.
    """
    for i, step in enumerate(steps):
        rel = {}
        src = step["source"]
        earlier = [r for r in results if r["step"] < i and r["id"] != src]
        cmd, args = command(step["_action"])
        if cmd in READ_COMMANDS:
            for r in earlier:
                if r["action"] == normalize(step["_action"]):
                    rel[r["id"]] = 2
        if cmd in PATH_COMMANDS and args:
            for r in earlier:
                rcmd, rargs = command(r["action"])
                if rcmd in PATH_COMMANDS and rargs and rargs[0] == args[0]:
                    rel.setdefault(r["id"], 1)
        query_tokens = distinctive(step["query"])
        for tok in distinctive(step["_thought"] + "\n" + step["_action"]):
            if tok in task_tokens or tok in stop_tokens or tok in query_tokens:
                continue
            hits = [r["id"] for r in earlier if tok in r["tokens"]]
            if 1 <= len(hits) <= MAX_REFERENCED:
                for h in hits:
                    rel.setdefault(h, 1)
        step["relevant"] = dict(sorted(rel.items()))


def finish_traj(traj, steps, results, task_text, stop_tokens):
    label_steps(steps, results, distinctive(task_text), stop_tokens)
    for s in steps:
        for k in [k for k in s if k.startswith("_")]:
            del s[k]
    traj["steps"] = steps
    return traj


def swe_agent(out):
    import pyarrow.parquet as pq
    split_of = splits("swe-agent")
    shards = sorted((DATA / "swe-agent").glob("train-*.parquet"))
    shards = shards[: int(os.environ.get("OOJURO_SWE_SHARDS", "1"))]
    stats = {"trajectories": 0, "steps": 0, "labelled_steps": 0, "records": 0, "truncated_records": 0,
             "shards": [p.name for p in shards]}
    rows_out = []
    for shard_no, path in enumerate(shards):
        table = pq.read_table(path, columns=["instance_id", "model_name", "trajectory"])
        for row_no, row in enumerate(table.to_pylist()):
            msgs = row["trajectory"]
            if len(msgs) < 3 or msgs[0]["role"] != "system" or msgs[1]["role"] != "user":
                continue
            traj_id = f"sa{shard_no}-{row_no}"
            system_text, task_text = msgs[0]["system_prompt"] or msgs[0]["text"] or "", msgs[1]["text"] or ""
            stop_tokens = distinctive(system_text)

            def rec(rid, kind, dt, text, meta=None):
                capped = cap_bytes(text, RECORD_CAP, True)
                stats["truncated_records"] += capped != text
                stats["records"] += 1
                return {"id": rid, "kind": kind, "dt": dt, "text": capped, "meta": meta or {}}

            init = [rec(f"{traj_id}/task", "task", 0, task_text)]
            steps, results = [], []
            prev_input, prev_source = task_text, init[0]["id"]
            body = msgs[2:]
            i = 0
            for j, m in enumerate(body):
                if m["role"] != "ai":
                    continue
                ai = m["text"] or ""
                obs = body[j + 1]["text"] if j + 1 < len(body) and body[j + 1]["role"] == "user" else None
                blocks = FENCE.findall(ai)
                action = blocks[-1].strip() if blocks else ""
                thought = FENCE.sub("", ai).strip() if blocks else ai.strip()
                cmd, args = command(action)
                meta = {"cmd": cmd} if cmd else {}
                if cmd in PATH_COMMANDS | {"create", "python"} and args:
                    meta["path"] = args[0]
                records = []
                if thought:
                    records.append(rec(f"{traj_id}/{i}/thinking", "thinking", 60 * i + 30, thought))
                if action:
                    records.append(rec(f"{traj_id}/{i}/call", "call", 60 * i + 31, action, dict(meta)))
                if obs is not None:
                    r = rec(f"{traj_id}/{i}/result", "result", 60 * i + 32, obs or "", dict(meta))
                    records.append(r)
                    results.append({"id": r["id"], "step": i, "action": normalize(action), "tokens": distinctive(r["text"])})
                steps.append({
                    "dt": 60 * i + 1,
                    "query": cap_bytes(prev_input, QUERY_CAP, False),
                    "source": prev_source,
                    "relevant": {},
                    "records": records,
                    "_thought": thought,
                    "_action": action,
                })
                if obs is not None:
                    prev_input, prev_source = obs or "", f"{traj_id}/{i}/result"
                elif records:
                    prev_input, prev_source = ai, records[-1]["id"]
                i += 1
            if not steps:
                continue
            traj = {"traj": traj_id, "group": row["instance_id"], "split": split_of.get(row["instance_id"], "heldout"),
                    "model": row["model_name"], "init": init}
            finish_traj(traj, steps, results, task_text, stop_tokens)
            stats["trajectories"] += 1
            stats["steps"] += len(steps)
            stats["labelled_steps"] += sum(1 for s in steps if s["relevant"])
            rows_out.append(traj)
    # Frozen order: by sha256 of the trajectory id, so "first N of a split" is a fixed sample.
    rows_out.sort(key=lambda t: hashlib.sha256(t["traj"].encode()).hexdigest())
    write_jsonl(out, rows_out)
    return stats


def hermes(out):
    split_of = splits("hermes-fc")
    data = json.loads((DATA / "hermes-fc" / "func-calling.json").read_text())
    stats = {"trajectories": 0, "steps": 0, "labelled_steps": 0, "records": 0}
    rows_out = []
    for conv in data:
        msgs = conv["conversations"]
        traj_id = f"hf-{conv['id']}"
        system = next((m["value"] for m in msgs if m["from"] == "system"), "")
        first_human = next((m["value"] for m in msgs if m["from"] == "human"), "")
        init = [{"id": f"{traj_id}/tools", "kind": "tools", "dt": 0, "text": system, "meta": {}}]
        steps, results, pending_human = [], [], []
        prev_input, prev_source = first_human, None
        i = 0
        for j, m in enumerate(msgs):
            if m["from"] == "human":
                rid = f"{traj_id}/h{j}"
                pending_human.append({"id": rid, "kind": "task", "dt": 60 * i + 2, "text": m["value"], "meta": {}})
                prev_input, prev_source = m["value"], rid
                continue
            if m["from"] != "gpt":
                continue
            out_text = m["value"]
            tool = msgs[j + 1]["value"] if j + 1 < len(msgs) and msgs[j + 1]["from"] == "tool" else None
            records = [{"id": f"{traj_id}/{i}/call", "kind": "call" if "<tool_call>" in out_text else "answer",
                        "dt": 60 * i + 31, "text": out_text, "meta": {}}]
            if tool is not None:
                records.append({"id": f"{traj_id}/{i}/result", "kind": "result", "dt": 60 * i + 32, "text": tool, "meta": {}})
                results.append({"id": records[-1]["id"], "step": i, "action": "", "tokens": distinctive(tool)})
            # A human turn is appended before the query it starts.
            if pending_human:
                if steps:
                    steps[-1]["records"].extend(pending_human)
                else:
                    init.extend(pending_human)
                pending_human = []
            steps.append({"dt": 60 * i + 3, "query": cap_bytes(prev_input, QUERY_CAP, False), "source": prev_source,
                          "relevant": {}, "records": records, "_thought": "", "_action": out_text})
            prev_input, prev_source = (tool, records[-1]["id"]) if tool is not None else (out_text, records[0]["id"])
            i += 1
        if not steps:
            continue
        stats["records"] += len(init) + sum(len(s["records"]) for s in steps)
        traj = {"traj": traj_id, "group": conv["id"], "split": split_of.get(conv["id"], "heldout"), "init": init}
        finish_traj(traj, steps, results, first_human, distinctive(system))
        stats["trajectories"] += 1
        stats["steps"] += len(steps)
        stats["labelled_steps"] += sum(1 for s in steps if s["relevant"])
        rows_out.append(traj)
    rows_out.sort(key=lambda t: hashlib.sha256(t["traj"].encode()).hexdigest())
    write_jsonl(out, rows_out)
    return stats


def unix(date):
    return int(datetime.strptime(date, "%Y/%m/%d (%a) %H:%M").replace(tzinfo=timezone.utc).timestamp())


def longmemeval(out):
    split_of = splits("longmemeval")
    data = json.loads((DATA / "longmemeval" / "longmemeval_s_cleaned.json").read_text())
    stats = {"questions": 0, "with_evidence": 0, "records": 0}
    rows_out = []
    for q in data:
        records, relevant, seen = [], {}, set()
        for sid, date, turns in zip(q["haystack_session_ids"], q["haystack_dates"], q["haystack_sessions"]):
            if sid in seen:  # 13 haystacks repeat one session verbatim; keep the first
                stats["duplicate_sessions_dropped"] = stats.get("duplicate_sessions_dropped", 0) + 1
                continue
            seen.add(sid)
            base = unix(date)
            for j, t in enumerate(turns):
                rid = f"{q['question_id']}/{sid}/{j}"
                records.append({"id": rid, "session": sid, "valid_at": base + j,
                                "text": f"{t['role']}: {t['content']}", "meta": {"role": t["role"]}})
                if t.get("has_answer"):
                    relevant[rid] = 1
        kind = "abstention" if q["question_id"].endswith("_abs") else q["question_type"]
        rows_out.append({"qid": q["question_id"], "type": kind, "split": split_of[q["question_id"]],
                         "question": q["question"], "at": unix(q["question_date"]), "records": records,
                         "relevant": relevant, "relevant_sessions": q["answer_session_ids"]})
        stats["questions"] += 1
        stats["with_evidence"] += bool(relevant)
        stats["records"] += len(records)
    # Frozen mixed order so --limit samples every question type.
    rows_out.sort(key=lambda r: hashlib.sha256(r["qid"].encode()).hexdigest())
    write_jsonl(out, rows_out)
    return stats


def write_jsonl(path, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".part")
    with open(tmp, "w", encoding="utf-8") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n")
    os.replace(tmp, path)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


JOBS = {("turnloop", "swe-agent"): swe_agent, ("turnloop", "hermes-fc"): hermes, ("memqa", "longmemeval"): longmemeval}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("suite", nargs="?")
    parser.add_argument("name", nargs="?")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    manifest = json.loads(MANIFEST.read_text()) if MANIFEST.exists() else {"schema": "oojuro-prepared-v1", "files": {}}
    if args.check:
        bad = 0
        for rel, entry in manifest["files"].items():
            path = DATA / "prepared" / rel
            ok = path.exists() and sha256(path) == entry["sha256"]
            bad += not ok
            print(("ok  " if ok else "BAD ") + rel)
        return 1 if bad else 0
    job = JOBS.get((args.suite, args.name))
    if not job:
        raise SystemExit(f"known: {sorted(JOBS)}")
    rel = f"{args.suite}/{args.name}.jsonl"
    out = DATA / "prepared" / rel
    stats = job(out)
    manifest["files"][rel] = {"sha256": sha256(out), "bytes": out.stat().st_size, "labels": LABELS,
                              "query_cap_bytes": QUERY_CAP, "record_cap_bytes": RECORD_CAP, "stats": stats}
    MANIFEST.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    print(json.dumps({rel: manifest["files"][rel]}, indent=2))


if __name__ == "__main__":
    sys.exit(main())
