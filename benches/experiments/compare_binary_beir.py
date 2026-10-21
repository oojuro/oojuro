"""Paired aggregate effects on already audited binary BEIR query packets.

The fixed percentile interval resamples queries, not documents, users or future
workloads. It describes this inspected test set and is not a production guarantee.
No query text, identity or source is written to the aggregate output.
"""

import argparse
import json
import math
import random
from pathlib import Path


def rows(directory, arm):
    values = json.loads((directory / f"{arm}-audit.json").read_text(encoding="utf-8"))
    assert len({value["query_id"] for value in values}) == len(values)
    return values


def effects(before, after):
    assert [row["query_id"] for row in before] == [row["query_id"] for row in after]
    metrics = {}
    for key in before[0]["quality"]:
        differences = [b["quality"][key] - a["quality"][key] for a, b in zip(before, after)]
        assert all(math.isfinite(value) for value in differences)
        metrics[key] = {
            "mean_before": math.fsum(row["quality"][key] for row in before) / len(before),
            "mean_after": math.fsum(row["quality"][key] for row in after) / len(after),
            "mean_delta": math.fsum(differences) / len(differences),
            "queries_higher": sum(value > 1e-12 for value in differences),
            "queries_equal": sum(abs(value) <= 1e-12 for value in differences),
            "queries_lower": sum(value < -1e-12 for value in differences),
        }
        if key == "ndcg_at10":
            generator = random.Random(20261019)
            samples = sorted(
                math.fsum(generator.choices(differences, k=len(differences))) / len(differences)
                for _ in range(5000)
            )
            metrics[key]["paired_query_bootstrap_95_percentile_interval"] = [samples[125], samples[4874]]
    return metrics


if __name__ == "__main__":
    if not __debug__:
        raise SystemExit("Run paired comparisons without Python optimization flags.")
    parser = argparse.ArgumentParser(description=__doc__)
    for argument in ["before-report", "before-audits", "after-report", "after-audits", "output"]:
        parser.add_argument(f"--{argument}", required=True, type=Path)
    args = parser.parse_args()
    assert not args.output.exists(), "Use a fresh aggregate output path"
    before = json.loads(args.before_report.read_text(encoding="utf-8"))
    after = json.loads(args.after_report.read_text(encoding="utf-8"))
    for key in ["dataset", "dataset_file_digests", "documents", "queries", "relevant_pairs", "sqlite_porter", "shared_query_stopwords"]:
        assert before[key] == after[key], "Different fixture or SQL control"
    for key in ["unavailable_qrels_included", "unavailable_relevant_pairs", "queries_with_unavailable_relevance", "unavailable_relevant_documents"]:
        assert before.get(key, 0) == after.get(key, 0), "Different unavailable-relevance policy"
    old_native = rows(args.before_audits, "nine")
    new_native = rows(args.after_audits, "nine")
    sql = rows(args.after_audits, "sqlite_fts5")
    old_sql = rows(args.before_audits, "sqlite_fts5")
    assert len(old_native) == len(new_native) == len(sql) == after["queries"]
    assert [(row["query_id"], row["candidate_ids"]) for row in old_sql] == [
        (row["query_id"], row["candidate_ids"]) for row in sql
    ], "SQL candidate order changed"
    output = {
        "dataset": after["dataset"],
        "queries": len(new_native),
        "new_native_vs_old_native": effects(old_native, new_native),
        "new_native_vs_sql": effects(sql, new_native),
        "bootstrap_seed": 20261019,
        "bootstrap_repetitions": 5000,
        "limitations": [
            "Inputs must first pass the independent binary rank/source-ID auditor.",
            "Intervals resample paired queries from already inspected data; related queries and shared documents can violate independence.",
            "Higher evidence bytes mean additional bytes, not higher quality. Neither bytes nor relevance labels score model tokens, supporting sentences, answers or agent outcomes.",
        ],
    }
    args.output.write_text(json.dumps(output, indent=2) + "\n", encoding="utf-8")
    print("Paired aggregate saved; queries:", len(new_native))
