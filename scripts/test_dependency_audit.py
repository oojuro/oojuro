#!/usr/bin/env python3
"""Exercise policy refusals using the real frozen selected graph; no Cargo/build needed."""
import copy
import gzip
import json
from pathlib import Path
import unittest

import dependency_audit as audit


class DependencyPolicy(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with gzip.open(Path(__file__).resolve().parents[1] / "docs/dependencies/main-native-after.json.gz", "rt") as source:
            cls.report = json.load(source)
        # Retain frozen compatibility graphs; the new file-engine scopes are checked independently.
        minimal = {"roots": ["ojo@0.1.0"], "packages": {
            "ojo@0.1.0": {"name": "ojo", "features": [], "used_by": []}}}
        for scope in ("default", "ojo", "ojo-all-platforms", "ojo-archive", "ojo-archive-all-platforms"):
            cls.report["graphs"][scope] = copy.deepcopy(minimal)

    def report_copy(self):
        return copy.deepcopy(self.report)

    def test_actual_selected_graph_is_allowed(self):
        audit.check(self.report)

    def test_provider_and_benchmark_dependencies_are_refused(self):
        for name in ["aws-sdk-s3", "rusqlite", "oojuro-bench"]:
            with self.subTest(name=name):
                report = self.report_copy()
                report["graphs"]["engine"]["packages"][name + "@1"] = {
                    "name": name, "features": [], "used_by": ["oojuro-engine@0.1.0"]}
                with self.assertRaisesRegex(ValueError, "outside engine allowlist"):
                    audit.check(report)

    def test_transitive_allowlist_does_not_permit_unreviewed_direct_edges(self):
        report = self.report_copy()
        package = next(p for p in report["graphs"]["engine"]["packages"].values() if p["name"] == "memchr")
        package["used_by"].append("oojuro-engine@0.1.0")
        with self.assertRaisesRegex(ValueError, "unreviewed direct"):
            audit.check(report)

    def test_lz4_frame_wrapper_is_refused(self):
        report = self.report_copy()
        package = next(p for p in report["graphs"]["store"]["packages"].values() if p["name"] == "lz4_flex")
        package["features"].append("frame")
        with self.assertRaisesRegex(ValueError, "block-only LZ4"):
            audit.check(report)

    def test_lz4_checked_decoder_cannot_be_disabled(self):
        report = self.report_copy()
        package = next(p for p in report["graphs"]["store"]["packages"].values() if p["name"] == "lz4_flex")
        package["features"].remove("checked-decode")
        with self.assertRaisesRegex(ValueError, "block-only LZ4"):
            audit.check(report)

    def test_store_reporting_json_cannot_leak_into_library(self):
        report = self.report_copy()
        package = copy.deepcopy(next(p for p in report["graphs"]["engine"]["packages"].values() if p["name"] == "serde_json"))
        package["used_by"] = ["oojuro-store@0.1.0"]
        report["graphs"]["store"]["packages"]["serde_json@1"] = package
        with self.assertRaisesRegex(ValueError, "benchmark JSON"):
            audit.check(report)

    def test_plain_build_does_not_select_cli(self):
        report = self.report_copy()
        report["graphs"]["default"]["roots"].append("oojuro@0.1.0")
        with self.assertRaisesRegex(ValueError, "only ojo"):
            audit.check(report)

    def test_minimal_core_refuses_codecs_and_benchmark_engines(self):
        for name in ("zstd", "rusqlite", "serde_json", "oojuro-engine"):
            with self.subTest(name=name):
                report = self.report_copy()
                report["graphs"]["ojo"]["packages"][name + "@1"] = {
                    "name": name, "features": [], "used_by": ["ojo@0.1.0"]}
                with self.assertRaisesRegex(ValueError, "outside minimal engine allowlist"):
                    audit.check(report)


if __name__ == "__main__":
    unittest.main()
