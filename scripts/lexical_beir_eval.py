"""Score a TREC run against BEIR qrels the standard BEIR/pytrec_eval way.

Exploration helper for the lexical track. Mirrors beir.retrieval.evaluation:
results whose document id equals the query id are dropped first
(ignore_identical_ids=True), then pytrec_eval computes ndcg_cut / recall.
Every labelled query counts, including ones with an empty run.

usage: python lexical_beir_eval.py QRELS_TSV RUN_FILE [--per-query OUT.json]
"""

import json
import sys
from collections import defaultdict

import pytrec_eval


def main() -> None:
    qrels_path, run_path = sys.argv[1], sys.argv[2]
    qrels: dict[str, dict[str, int]] = defaultdict(dict)
    with open(qrels_path) as f:
        next(f)
        for line in f:
            qid, did, grade = line.rstrip("\n").split("\t")
            qrels[qid][did] = int(grade)
    run: dict[str, dict[str, float]] = {qid: {} for qid in qrels}
    with open(run_path) as f:
        for line in f:
            qid, _, did, rank, score, _ = line.split()
            if qid in run and did != qid:
                # Ranks, not raw scores, define the order (ties already broken).
                run[qid][did] = -float(rank)
    # Keep at most 100 results after the identical-id drop.
    for qid, docs in run.items():
        if len(docs) > 100:
            keep = sorted(docs.items(), key=lambda kv: -kv[1])[:100]
            run[qid] = dict(keep)
    measures = {"ndcg_cut.10", "recall.10", "recall.100"}
    evaluator = pytrec_eval.RelevanceEvaluator(dict(qrels), measures)
    scores = evaluator.evaluate(run)
    n = len(qrels)
    agg = {
        "queries": n,
        "ndcg@10": sum(s["ndcg_cut_10"] for s in scores.values()) / n,
        "recall@10": sum(s["recall_10"] for s in scores.values()) / n,
        "recall@100": sum(s["recall_100"] for s in scores.values()) / n,
    }
    print(json.dumps({k: round(v, 4) if isinstance(v, float) else v for k, v in agg.items()}))
    if "--per-query" in sys.argv:
        out = sys.argv[sys.argv.index("--per-query") + 1]
        with open(out, "w") as f:
            json.dump({q: s["ndcg_cut_10"] for q, s in scores.items()}, f)


if __name__ == "__main__":
    main()
