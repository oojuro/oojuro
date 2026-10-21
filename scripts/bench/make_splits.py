#!/usr/bin/env python3
"""Write frozen dev/held-out ID lists to bench/splits/<dataset>.json.

Rule (oojuro-split-v1): an id goes to dev iff
    int(sha256("oojuro-split-v1:<dataset>:<id>").hexdigest()[:8], 16) % 3 == 0
else held-out. Tune only on dev; report final numbers on held-out.
Units: BEIR = test query id; swe-agent = instance_id (all model runs of one
issue share a split); longmemeval = question_id; hermes-fc = conversation id.
Existing split files are never rewritten unless --force.

Usage: $OOJURO_DATA/.venv/bin/python -I scripts/bench/make_splits.py [--force] NAME...|all
"""

import argparse
import csv
import hashlib
import json
import os
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
OUT = REPO / "bench" / "splits"
DATA = Path(os.environ.get("OOJURO_DATA", "~/.cache/oojuro-data")).expanduser()


def is_dev(dataset, ident):
    h = hashlib.sha256(f"oojuro-split-v1:{dataset}:{ident}".encode()).hexdigest()
    return int(h[:8], 16) % 3 == 0


def beir_ids(name):
    short = name.removeprefix("beir-")
    path = DATA / name / short / "qrels" / "test.tsv"
    ids = set()
    with open(path, newline="") as f:
        rows = csv.reader(f, delimiter="\t")
        next(rows)
        for qid, _, score in rows:
            if int(score) > 0:
                ids.add(qid)
    return ids, "BEIR test query id with >=1 positive qrel"


def swe_agent_ids(name):
    import pyarrow.parquet as pq
    ids = set()
    for p in sorted((DATA / name).glob("*.parquet")):
        ids.update(pq.read_table(p, columns=["instance_id"]).column("instance_id").to_pylist())
    return ids, "instance_id (every model's trajectory for one issue shares the split)"


def longmemeval_ids(name):
    data = json.loads((DATA / name / "longmemeval_s_cleaned.json").read_text())
    return {q["question_id"] for q in data}, "question_id (longmemeval_s)"


def hermes_ids(name):
    data = json.loads((DATA / name / "func-calling.json").read_text())
    return {row["id"] for row in data}, "conversation id (func-calling.json)"


SOURCES = {
    "beir-scifact": beir_ids, "beir-fiqa": beir_ids, "beir-arguana": beir_ids,
    "beir-scidocs": beir_ids, "beir-nfcorpus": beir_ids,
    "swe-agent": swe_agent_ids, "longmemeval": longmemeval_ids, "hermes-fc": hermes_ids,
}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("names", nargs="+")
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()
    names = list(SOURCES) if args.names == ["all"] else args.names
    OUT.mkdir(parents=True, exist_ok=True)
    for name in names:
        target = OUT / f"{name}.json"
        if target.exists() and not args.force:
            print(f"keep {target} (frozen)")
            continue
        ids, unit = SOURCES[name](name)
        ordered = sorted(ids)
        dev = [i for i in ordered if is_dev(name, i)]
        heldout = [i for i in ordered if not is_dev(name, i)]
        body = {
            "schema": "oojuro-split-v1",
            "dataset": name,
            "rule": "dev iff int(sha256('oojuro-split-v1:<dataset>:<id>')[:8],16) % 3 == 0",
            "unit": unit,
            "counts": {"dev": len(dev), "heldout": len(heldout)},
            "sha256_of_ids": hashlib.sha256("\n".join(ordered).encode()).hexdigest(),
            "dev": dev,
            "heldout": heldout,
        }
        target.write_text(json.dumps(body, indent=0) + "\n")
        print(f"wrote {target}: dev {len(dev)} heldout {len(heldout)}")


if __name__ == "__main__":
    sys.exit(main())
