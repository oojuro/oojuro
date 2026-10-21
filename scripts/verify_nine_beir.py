"""Recompute binary BEIR rank/source-ID metrics without model dependencies.

This checks canonical labels, candidate identities, identical-ID exclusion and
macro averages. Saved flat audits omit excerpt ranges/text: their byte counts
are only bounded here, not independently reconstructed. This is neither a
supporting-sentence nor a generated-answer accuracy test.
"""

import argparse
import csv
import json
import math
from collections import defaultdict
from pathlib import Path

if not __debug__:
    raise SystemExit("Run the ranking verifier without Python optimization flags.")


def verify(report_path, audit_directory, dataset):
    report = json.loads(report_path.read_text(encoding="utf-8"))
    assert report["context_encoding"] == "flat-snippets-v1", "Expected flat source audits"
    assert report["changed_repeat_packets"] == 0, "Repeated packets changed"
    assert report["embedding_calls"] == report["llm_calls"] == 0, "Unexpected provider calls"
    assert report["ignore_identical_ids"] is True, "Missing identical-ID policy"
    assert report["answer_accuracy"] is report["model_input_tokens"] is None
    labels = defaultdict(set)
    with (dataset / "qrels/test.tsv").open(encoding="utf-8") as stream:
        for row in csv.DictReader(stream, delimiter="\t"):
            assert row["score"] == "1", "Expected binary positive qrels"
            labels[row["query-id"]].add(row["corpus-id"])
    assert labels and report["queries"] == len(labels), "Labeled query count mismatch"
    assert report["relevant_pairs"] == sum(map(len, labels.values()))
    documents = set()
    with (dataset / "corpus.jsonl").open(encoding="utf-8") as stream:
        for line in stream:
            identity = json.loads(line)["_id"]
            assert identity not in documents, "Duplicate corpus document"
            documents.add(identity)
    assert report["documents"] == report["exact_source_cross_checks"] == len(documents)
    unavailable = [relevant - documents for relevant in labels.values()]
    counts = {
        "unavailable_relevant_pairs": sum(map(len, unavailable)),
        "queries_with_unavailable_relevance": sum(bool(ids) for ids in unavailable),
        "unavailable_relevant_documents": len(set().union(*unavailable)),
    }
    assert not counts["unavailable_relevant_pairs"] or report.get("unavailable_qrels_included") is True, "Unavailable positives were not explicitly allowed"
    for name, count in counts.items():
        assert report.get(name, 0) == count, "Unavailable relevance count mismatch"
    assert report["query_document_id_collisions"] == len(labels.keys() & documents)
    query_ids = []
    with (dataset / "queries.jsonl").open(encoding="utf-8") as stream:
        for line in stream:
            identity = json.loads(line)["_id"]
            if identity in labels:
                query_ids.append(identity)
    assert len(query_ids) == len(set(query_ids)) == len(labels)
    assert set(query_ids) == labels.keys(), "Missing labeled query text"

    def identities(values):
        assert all(value.startswith("beir:") for value in values), "Unexpected source namespace"
        raw = [value.removeprefix("beir:") for value in values]
        assert len(set(raw)) == len(raw), "Duplicate returned source"
        assert set(raw) <= documents, "Unknown returned source"
        return raw

    packets = 0
    for arm in ["nine", "sqlite_fts5"]:
        rows = json.loads((audit_directory / f"{arm}-audit.json").read_text(encoding="utf-8"))
        assert len(rows) == len(labels)
        assert [row["query_id"] for row in rows] == query_ids, "Query order changed"
        metrics = defaultdict(list)
        for row in rows:
            query = row["query_id"]
            relevant = labels[query]
            candidates = identities(row["candidate_ids"])
            assert len(candidates) <= report["candidate_depth"]
            ranked = [source for source in candidates if source != query]
            selected = set(identities(row["context_ids"]))
            assert query not in selected, "Identical-ID source entered context"
            assert selected <= set(ranked), "Context source missing from candidate ranks"
            assert len(selected) <= report["context_limit"]
            relevant_ranks = [i + 1 for i, source in enumerate(ranked) if source in relevant]
            dcg = math.fsum(1 / math.log2(i + 1) for i in relevant_ranks if i <= 10)
            ideal = math.fsum(1 / math.log2(i + 1) for i in range(1, min(10, len(relevant)) + 1))
            measured = {
                "ndcg_at10": dcg / ideal,
                "mrr_at10": 1 / min(relevant_ranks) if relevant_ranks and min(relevant_ranks) <= 10 else 0,
                "recall_at10": sum(i <= 10 for i in relevant_ranks) / len(relevant),
                "recall_at32": sum(i <= 32 for i in relevant_ranks) / len(relevant),
                "context_document_recall": len(selected & relevant) / len(relevant),
                "all_relevant_documents_in_context": int(relevant <= selected),
                "evidence_bytes": row["quality"]["evidence_bytes"],
            }
            assert type(measured["evidence_bytes"]) is int
            assert 0 <= measured["evidence_bytes"] <= report["context_budget_bytes"]
            for name, value in measured.items():
                assert math.isfinite(value)
                assert abs(value - row["quality"][name]) < 1e-12, "Per-query metric mismatch"
                metrics[name].append(value)
        for name, values in metrics.items():
            assert abs(math.fsum(values) / len(values) - report["arms"][arm]["quality"][name]) < 1e-10, "Aggregate metric mismatch"
        packets += len(rows)
    print("Independent binary rank/source-ID audit passed; packets:", packets)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", required=True, type=Path)
    parser.add_argument("--audits", required=True, type=Path)
    parser.add_argument("--dataset", required=True, type=Path)
    arguments = parser.parse_args()
    verify(arguments.report, arguments.audits, arguments.dataset)
