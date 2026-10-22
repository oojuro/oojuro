"""Aggregate cached D18-D24 result JSONs without importing research or corpus code.

Only aggregate statistics leave this process. A reused evaluation set always yields
exploratory evidence; this tool never issues an adoption or heldout verdict.
"""
import argparse
import collections
import hashlib
import json
import math
from pathlib import Path
import random
import sys

SCENARIOS = ("own", "start@1M", "start@10M", "start@100M", "start", "ood", "adapt", "pool")
ARMS = ("V", "X", "T", "H1", "H2", "H3", "H3b")
SEED = 20261021


def score(row, arm):
    value = row.get(arm)
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or not 0 <= value <= 1:
        raise ValueError("invalid or missing recall")
    return float(value)


def percentile(values, p):
    values = sorted(values)
    pos = (len(values) - 1) * p
    lo = math.floor(pos)
    hi = math.ceil(pos)
    return values[lo] + (values[hi] - values[lo]) * (pos - lo)


def paired_summary(values, baseline, dense, repeats):
    # Sample question indices once per resample; every difference stays paired.
    rng = random.Random(SEED)
    n = len(values)
    draws = ([], [], [])
    for _ in range(repeats):
        ids = [rng.randrange(n) for _ in range(n)]
        sums = (sum(values[i] for i in ids),
                sum(values[i] - baseline[i] for i in ids),
                sum(dense[i] - values[i] for i in ids))
        for output, total in zip(draws, sums):
            output.append(total / n)
    means = (sum(values) / n,
             sum(a - b for a, b in zip(values, baseline)) / n,
             sum(e - a for e, a in zip(dense, values)) / n)
    return {key: {"mean": mean, "ci95": [percentile(draw, .025), percentile(draw, .975)]}
            for key, mean, draw in zip(("recall", "minus_O8", "E8_minus"), means, draws)}


def summarize(result_dir, repeats=10000):
    if repeats < 100:
        raise ValueError("at least 100 bootstrap resamples required")
    records = {}
    missing = []
    for name in (*SCENARIOS, "compress"):
        path = Path(result_dir) / (name.replace("@", "_") + ".json")
        if not path.is_file():
            missing.append(name)
            continue
        raw = path.read_bytes()
        data = json.loads(raw)
        if not isinstance(data, dict) or data.get("scenario") != name or data.get("dev_n") != 103:
            raise ValueError("scenario identity or full development count does not match")
        evaluation = data.get("eval", {})
        if not isinstance(evaluation, dict) or len(evaluation) != 90 or any(not isinstance(r, dict) for r in evaluation.values()):
            raise ValueError("expected 90 evaluation rows")
        counts = collections.Counter(r.get("bench") for r in evaluation.values())
        if counts != {"lme_m": 45, "beam_1M": 45}:
            raise ValueError("benchmark split does not match")
        for row in evaluation.values():
            score(row, "O")
            if name != "compress":
                for arm in (*ARMS, "E_stored", "O_stored"):
                    score(row, arm)
                if abs(row["O"] - row["O_stored"]) > 1e-10:
                    raise ValueError("O8 reference did not reproduce")
            else:
                score(row, "M3")
        records[name] = (data, hashlib.sha256(raw).hexdigest())
    scenarios = [name for name in records if name != "compress"]
    if not scenarios:
        raise ValueError("no scenario with dense references available")
    reference = records[scenarios[0]][0]["eval"]
    qids = sorted(reference)
    baseline = [score(reference[q], "O") for q in qids]
    dense = [score(reference[q], "E_stored") for q in qids]
    output = {"status": "incomplete" if missing else "complete_cached_matrix",
              "evidence": "exploratory_reused_evaluation", "adoption_verdict": None,
              "evaluation_n": 90, "development_n": 103,
              "bootstrap": {"resamples": repeats, "seed": SEED, "unit": "question", "implementation": "Python stdlib random"},
              "missing_scenarios": missing,
              "references": {"O8": sum(baseline) / 90, "E8": sum(dense) / 90}, "scenarios": {}}
    for name, (data, digest) in records.items():
        rows = data["eval"]
        if set(rows) != set(qids):
            raise ValueError("question sets do not match across scenarios")
        for i, q in enumerate(qids):
            if rows[q]["bench"] != reference[q]["bench"]:
                raise ValueError("question benchmark changed across scenarios")
            if abs(score(rows[q], "O") - baseline[i]) > 1e-10:
                raise ValueError("O8 reference changed across scenarios")
            if name != "compress" and abs(score(rows[q], "E_stored") - dense[i]) > 1e-10:
                raise ValueError("E8 reference changed across scenarios")
        arms = ("M3",) if name == "compress" else ARMS
        headline = "M3" if name == "compress" else data.get("headline")
        if headline not in arms or (name != "compress" and headline not in ("V", "X", "T", "H1")):
            raise ValueError("invalid development-selected headline")
        if name != "compress":
            selected = data.get("selected", {})
            if not isinstance(selected, dict) or set(selected) != {"V", "X", "T", "H1"} or any(not isinstance(v, dict) for v in selected.values()):
                raise ValueError("missing development selections")
            dev_scores = {a: selected[a].get("dev_recall") for a in selected}
            if any(isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v) or not 0 <= v <= 1 for v in dev_scores.values()):
                raise ValueError("invalid development recall")
            expected = max(("V", "X", "T", "H1"), key=lambda a: dev_scores[a])
            if headline != expected:
                raise ValueError("headline does not match development selection")
        means = {}
        for arm in arms:
            means[arm] = {bench: sum(score(rows[q], arm) for q in qids if bench == "pooled" or rows[q]["bench"] == bench) / (90 if bench == "pooled" else 45)
                          for bench in ("pooled", "lme_m", "beam_1M")}
        output["scenarios"][name] = {"source_sha256": digest, "headline": headline,
            "headline_paired": paired_summary([score(rows[q], headline) for q in qids], baseline, dense, repeats),
            "arm_means": means, "selected": data["selected"]}
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("result_dir")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        result = summarize(args.result_dir)
        text = json.dumps(result, indent=2, allow_nan=False) + "\n"
        if args.output:
            # Non-overwrite: preserve existing research reports.
            with args.output.open("x") as target:
                target.write(text)
        else:
            print(text, end="")
    except (OSError, ValueError, KeyError, TypeError) as exc:
        # Avoid printing paths, malformed records, or individual question ids.
        print("Cannot summarize result artifacts: " + (str(exc) if isinstance(exc, ValueError) and not isinstance(exc, json.JSONDecodeError) else type(exc).__name__), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
