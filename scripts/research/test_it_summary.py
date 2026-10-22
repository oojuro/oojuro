import contextlib
import copy
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import it_summary as S


def fixture():
    selected = {a: {"config": "synthetic", "dev_recall": .6 if a == "H1" else .5} for a in ("V", "X", "T", "H1")}
    rows = {f"synthetic-question-{i}": {"bench": "lme_m" if i < 45 else "beam_1M", "O": .5, "O_stored": .5, "E_stored": .8,
                  **{a: .7 for a in S.ARMS}} for i in range(90)}
    return {"scenario": "own", "dev_n": 103, "headline": "H1", "selected": selected, "eval": rows}


class SummaryTests(unittest.TestCase):
    def run_fixture(self, data, mutate=None):
        with tempfile.TemporaryDirectory() as folder:
            p = Path(folder) / "own.json"
            p.write_text(json.dumps(data))
            if mutate:
                mutate(Path(folder))
            return S.summarize(folder, repeats=100)

    def test_aggregate_only_and_incomplete_is_exploratory(self):
        out = self.run_fixture(fixture())
        self.assertEqual(out["status"], "incomplete")
        self.assertEqual(out["evidence"], "exploratory_reused_evaluation")
        self.assertIsNone(out["adoption_verdict"])
        self.assertEqual(out["missing_scenarios"], [s for s in (*S.SCENARIOS, "compress") if s != "own"])
        self.assertNotIn("synthetic-question", json.dumps(out))
        paired = out["scenarios"]["own"]["headline_paired"]
        self.assertAlmostEqual(paired["minus_O8"]["mean"], .2)
        self.assertAlmostEqual(paired["E8_minus"]["mean"], .1)

    def test_smoke_development_count_rejected(self):
        data = fixture(); data["dev_n"] = 12
        with self.assertRaisesRegex(ValueError, "development count"):
            self.run_fixture(data)

    def test_mismatched_question_set_rejected(self):
        def add(folder):
            data = fixture(); data["scenario"] = "start"
            data["eval"]["different-id"] = data["eval"].pop("synthetic-question-0")
            (folder / "start.json").write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "question sets"):
            self.run_fixture(fixture(), add)

    def test_nonfinite_score_and_changed_reference_rejected(self):
        for value in (float("nan"), 1.01, True, "0.5"):
            data = fixture(); data["eval"]["synthetic-question-0"]["H1"] = value
            with self.assertRaisesRegex(ValueError, "recall"):
                self.run_fixture(data)
        data = fixture(); data["eval"]["synthetic-question-0"]["O_stored"] = .4
        with self.assertRaisesRegex(ValueError, "reference"):
            self.run_fixture(data)

    def test_hindsight_headline_rejected(self):
        data = fixture(); data["headline"] = "V"
        with self.assertRaisesRegex(ValueError, "development selection"):
            self.run_fixture(data)

    def test_cli_rejects_malformed_json_without_echoing_private_data(self):
        with tempfile.TemporaryDirectory() as folder:
            (Path(folder) / "own.json").write_text("private-question-content")
            error = io.StringIO()
            with patch("sys.argv", ["it_summary.py", folder]), contextlib.redirect_stderr(error):
                self.assertEqual(S.main(), 1)
            self.assertNotIn(folder, error.getvalue())
            self.assertNotIn("private-question-content", error.getvalue())


if __name__ == "__main__":
    unittest.main()
