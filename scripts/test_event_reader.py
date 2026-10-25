#!/usr/bin/env python3
"""Independent Event profile reader conformance checks; synthetic data only."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("read_events", ROOT / "examples/read_events.py")
READER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(READER)


class EventReaderTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="ojo-event-reader-")
        self.addCleanup(self.scratch.cleanup)
        self.path = Path(self.scratch.name) / "journal.ojo"
        self.vectors = json.loads((ROOT / "spec/events/1/vectors.json").read_text())["valid"]

    def test_independent_vectors(self):
        for vector in self.vectors:
            self.path.write_bytes(bytes.fromhex(vector["file_hex"]))
            report = READER.read_events(self.path)
            self.assertEqual(report["events"], vector["events"])
            self.assertEqual(report["incomplete_tail_bytes"], 0)

    def test_each_committed_corruption_refused_without_repair(self):
        raw = bytes.fromhex(self.vectors[-1]["file_hex"])
        for at in range(len(raw)):
            changed = raw[:at] + bytes([raw[at] ^ 128]) + raw[at + 1:]
            self.path.write_bytes(changed)
            with self.assertRaises(ValueError, msg=f"corrupt byte {at}"):
                READER.read_events(self.path)
            self.assertEqual(self.path.read_bytes(), changed)

    def test_incomplete_tail_is_reported_and_never_modified(self):
        raw = bytes.fromhex(self.vectors[0]["file_hex"])
        tail = b"partial transaction"
        self.path.write_bytes(raw + tail)
        report = READER.read_events(self.path)
        self.assertEqual(report["events"], [])
        self.assertEqual(report["incomplete_tail_bytes"], len(tail))
        self.assertEqual(self.path.read_bytes(), raw + tail)


if __name__ == "__main__":
    unittest.main()
