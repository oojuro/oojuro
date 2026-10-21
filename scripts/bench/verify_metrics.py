#!/usr/bin/env python3
"""Cross-check the harness's BEIR metrics against pytrec_eval (trec_eval).

Usage:
  oojuro-bench beir --dataset beir-scifact --split test --runs 1 --run-file /path/x.run --out x.json
  $OOJURO_DATA/.venv/bin/python -I scripts/bench/verify_metrics.py x.json /path/x.run.<label>...

Run files carry unique descending scores (k - rank), so trec_eval's own
score-then-docid sort reproduces the arm's order exactly. Queries the arm
returned nothing for are scored 0 here, as the harness does (pytrec_eval
alone would drop them). Exits 1 if any metric differs by more than 1e-9.
"""

import json
import os
import sys
from collections import defaultdict
from pathlib import Path

import pytrec_eval

DATA = Path(os.environ.get("OOJURO_DATA", "~/.cache/oojuro-data")).expanduser()
REPO = Path(__file__).resolve().parents[2]


def main():
    card = json.loads(Path(sys.argv[1]).read_text())
    dataset, split = card["dataset"], card["split"]
    short = dataset.removeprefix("beir-")
    qrels = defaultdict(dict)
    with open(DATA / dataset / short / "qrels" / "test.tsv") as f:
        next(f)
        for line in f:
            q, d, s = line.rstrip("\n").split("\t")
            qrels[q][d] = int(s)
    wanted = {q for q, rel in qrels.items() if any(v > 0 for v in rel.values())}
    if split not in ("test", "all"):
        wanted &= set(json.loads((REPO / "bench" / "splits" / f"{dataset}.json").read_text())[split])
    bad = 0
    for run_path in sys.argv[2:]:
        label = run_path.rsplit(".", 1)[-1]
        arm = next(a for a in card["arms"] if a["label"] == label)
        run, run10 = defaultdict(dict), defaultdict(dict)
        with open(run_path) as f:
            for line in f:
                q, _, d, rank, score, _ = line.split()
                run[q][d] = float(score)
                if int(rank) <= 10:
                    run10[q][d] = float(score)
        sub = {q: qrels[q] for q in wanted}
        full = pytrec_eval.RelevanceEvaluator(sub, {"ndcg_cut.10", "recall.10,100"}).evaluate({q: run[q] for q in wanted if q in run})
        top = pytrec_eval.RelevanceEvaluator(sub, {"recip_rank"}).evaluate({q: run10[q] for q in wanted if q in run10})
        ours = {
            "ndcg@10": sum(full.get(q, {}).get("ndcg_cut_10", 0.0) for q in wanted) / len(wanted),
            "recall@10": sum(full.get(q, {}).get("recall_10", 0.0) for q in wanted) / len(wanted),
            "recall@100": sum(full.get(q, {}).get("recall_100", 0.0) for q in wanted) / len(wanted),
            "mrr@10": sum(top.get(q, {}).get("recip_rank", 0.0) for q in wanted) / len(wanted),
        }
        for metric, ref in ours.items():
            got = arm["quality"][metric]["runs"][0]
            ok = abs(got - ref) <= 1e-9
            bad += not ok
            print(f"{'ok ' if ok else 'BAD'} {dataset} {split} {label:10s} {metric:11s} harness={got:.6f} pytrec_eval={ref:.6f}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
