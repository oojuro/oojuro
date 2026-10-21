"""Flatten public agent trajectories into ordered turn records (exploration).

Exploration helper for the lexical track's turn-loop probe; the shared
harness on track/bench owns the comparable benchmark. Output is JSONL, one
record per line, in replay order:

  {"traj": str, "turn": int, "kind": "task|thinking|tool_call|tool_result",
   "text": str}

Supported inputs (Hugging Face parquet shards, checksummed by the caller):
  swe-agent   nebius/SWE-agent-trajectories (role system/user/ai)
  openhands   SWE-Gym/OpenHands-Sampled-Trajectories (OpenAI-style messages)

usage: python -I lexical_turns.py FORMAT IN.parquet OUT.jsonl [MAX_TRAJ]
"""

import json
import sys

import pyarrow.parquet as pq


def swe_agent(row):
    traj = row["instance_id"] + "/" + row["model_name"]
    turn = 0
    for m in row["trajectory"] or []:
        role, text = m["role"], m["text"] or ""
        if role == "system":
            continue
        if role == "user" and turn == 0:
            yield traj, turn, "task", text
            turn += 1
        elif role == "ai":
            # SWE-agent replies are free-text reasoning ending in one command.
            fence = text.rfind("```")
            start = text.rfind("```", 0, fence) if fence > 0 else -1
            if start >= 0:
                yield traj, turn, "thinking", text[:start].rstrip()
                yield traj, turn, "tool_call", text[start:].strip()
            else:
                yield traj, turn, "thinking", text
        elif role == "user":
            yield traj, turn, "tool_result", text
            turn += 1


def openhands(row):
    traj = row["instance_id"] + "/" + row["run_id"]
    turn = 0
    for m in row["messages"] or []:
        role, content = m["role"], m["content"] or ""
        if role == "system":
            continue
        if role == "user" and turn == 0:
            yield traj, turn, "task", content
            turn += 1
        elif role == "assistant":
            if content:
                yield traj, turn, "thinking", content
            for call in m["tool_calls"] or []:
                fn = call["function"]
                yield traj, turn, "tool_call", json.dumps(
                    {"name": fn["name"], "arguments": fn["arguments"]}, ensure_ascii=False
                )
        elif role in ("tool", "user"):
            yield traj, turn, "tool_result", content
            turn += 1


def main() -> None:
    fmt, src, dst = sys.argv[1], sys.argv[2], sys.argv[3]
    limit = int(sys.argv[4]) if len(sys.argv) > 4 else None
    walk = {"swe-agent": swe_agent, "openhands": openhands}[fmt]
    table = pq.read_table(src)
    n = 0
    with open(dst, "w") as out:
        for batch in table.to_batches(max_chunksize=64):
            for row in batch.to_pylist():
                if limit is not None and n >= limit:
                    return
                n += 1
                for traj, turn, kind, text in walk(row):
                    if text:
                        out.write(
                            json.dumps(
                                {"traj": traj, "turn": turn, "kind": kind, "text": text},
                                ensure_ascii=False,
                            )
                            + "\n"
                        )


if __name__ == "__main__":
    main()
