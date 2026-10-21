"""Recompute evidence completeness from exact SQLite originals and saved ranges."""
import argparse
import json
import sqlite3
from collections import defaultdict
from pathlib import Path

if not __debug__:
    raise SystemExit("Run the evidence verifier without Python optimization flags.")
p = argparse.ArgumentParser(description="Verify Nine exact-range benchmark audits against frozen SQLite originals.")
p.add_argument("--report", required=True, type=Path)
p.add_argument("--audits", required=True, type=Path)
p.add_argument("--sqlite", required=True, type=Path)
p.add_argument("--cases", required=True, type=Path)
a = p.parse_args()
report = json.loads(a.report.read_text(encoding="utf-8"))
cases = json.loads(a.cases.read_text(encoding="utf-8"))
db = sqlite3.connect(a.sqlite.resolve().as_uri() + "?mode=ro", uri=True)
assert report["corpus_controls"]["changed_repeat_packets"] == 0
assert report["embedding_calls"] == report["llm_calls"] == 0
assert report["sqlite_full_body_fallbacks"] == 0
originals = {}
for c in cases:
    for e in c["case"]["evidence"]:
        sid = e["source_id"]
        if sid not in originals:
            row = db.execute("select body from sources where id=?", (sid,)).fetchone()
            assert row is not None
            originals[sid] = row[0].encode()
        assert e["quote"].encode() in originals[sid]
        source_key, valid, known = db.execute("select source_key,valid_at,known_at from sources where id=?", (sid,)).fetchone()
        at = c["case"]["at"]
        assert valid <= at and known <= at
        winner, = db.execute("select id from sources where source_key=?1 and valid_at<=?2 and known_at<=?2 order by valid_at desc,known_at desc,rowid desc limit 1", (source_key, at)).fetchone()
        assert winner == sid

packets = 0
for arm, control in report["corpus_controls"]["arms"].items():
    rows = json.loads((a.audits / f"{arm}.json").read_text(encoding="utf-8"))
    assert len(rows) == len(cases)
    groups = defaultdict(lambda: defaultdict(float))
    for row, labeled in zip(rows, cases):
        c = labeled["case"]
        assert row["case_id"] == c["id"]
        assert len(set(row["candidate_ids"])) == len(row["candidate_ids"])
        assert len(set(row["context_ids"])) == len(row["context_ids"])
        spans = dict(row["source_ranges"])
        assert set(spans) == set(row["context_ids"])
        assert row["serialized_context_bytes"] <= control["context_budget_bytes"]
        wire = row.get("encoded_context")
        if wire is None:
            assert len(spans) <= control["k"]
            expected_wire = []
            for sid, (start, end) in row["source_ranges"]:
                text, key, valid, known, verified, size = db.execute("select substr(CAST(body AS BLOB),?1+1,?2-?1),source_key,valid_at,known_at,verified,body_bytes from sources where id=?3", (start, end, sid)).fetchone()
                assert 0 <= start <= end <= size and len(text) == end - start
                assert valid <= c["at"] and known <= c["at"]
                winner, = db.execute("select id from sources where source_key=?1 and valid_at<=?2 and known_at<=?2 order by valid_at desc,known_at desc,rowid desc limit 1", (key, c["at"])).fetchone()
                assert winner == sid
                expected_wire.append({"id": sid, "key": key, "text": text.decode("utf-8"), "valid_at": valid, "recorded_at": known, "verified": bool(verified), "source_range": [start, end]})
            assert len(json.dumps(expected_wire, ensure_ascii=False, separators=(",", ":")).encode()) == row["serialized_context_bytes"]
        else:
            assert report["context_limit_unit"] in ["distinct_text_blocks", "source_origins"]
            if report["context_limit_unit"] == "source_origins":
                assert len(wire) <= control["k"] and all(not g.get("also_from") for g in wire)
            count = sum(1 + len(g.get("also_from", [])) for g in wire)
            aliases = {}
            for group in wire:
                for alias in group.get("also_from", []):
                    at = alias["at"]
                    assert type(at) is int and 0 < at < count and at not in aliases
                    assert alias["text"] == ""
                    restored = {k: v for k, v in alias.items() if k != "at"}
                    restored["text"] = group["text"]
                    aliases[at] = restored
            primary_slots = iter(i for i in range(count) if i not in aliases)
            ordered = list(aliases.items())
            ordered.extend((next(primary_slots), {k: v for k, v in group.items() if k != "also_from"}) for group in wire)
            decoded = [s for _, s in sorted(ordered)]
            assert [s["id"] for s in decoded] == row["context_ids"]
            assert [[s["id"], s["source_range"]] for s in decoded] == row["source_ranges"]
            assert len({s["text"] for s in decoded}) <= control["k"]
            assert len(decoded) <= len(row["candidate_ids"])
            ranks = [row["candidate_ids"].index(s["id"]) for s in decoded]
            assert ranks == sorted(ranks)
            assert len(json.dumps(wire, ensure_ascii=False, separators=(",", ":")).encode()) == row["serialized_context_bytes"]
            for s in decoded:
                start, end = s["source_range"]
                text, key, valid, known, verified = db.execute("select substr(CAST(body AS BLOB),?1+1,?2-?1),source_key,valid_at,known_at,verified from sources where id=?3", (start, end, s["id"])).fetchone()
                assert text == s["text"].encode() and len(text) == end - start
                assert (key, valid, known, bool(verified)) == (s["key"], s["valid_at"], s["recorded_at"], s["verified"])
                assert valid <= c["at"] and known <= c["at"]
                winner, = db.execute("select id from sources where source_key=?1 and valid_at<=?2 and known_at<=?2 order by valid_at desc,known_at desc,rowid desc limit 1", (key, c["at"])).fetchone()
                assert winner == s["id"]
        found = {}
        for sid in {e["source_id"] for e in c["evidence"]}:
            if sid in spans:
                start, end = spans[sid]
                assert 0 <= start <= end <= len(originals[sid])
                found[sid] = originals[sid][start:end]
                found[sid].decode("utf-8")
        refs = {e["source_id"] for e in c["evidence"]}
        retained = sum(e["quote"].encode() in found.get(e["source_id"], b"") for e in c["evidence"])
        g = groups[labeled["group"]]
        g["questions"] += 1
        g["reference_pairs"] += len(refs)
        g["pairs_in_context"] += len(refs & found.keys())
        g["pairs_in_candidate_pool"] += len(refs & set(row["candidate_ids"]))
        g["pairs_with_all_reference_quotes"] += sum(all(e["quote"].encode() in found.get(sid, b"") for e in c["evidence"] if e["source_id"] == sid) for sid in refs)
        g["reference_quotes_in_context"] += retained
        g["questions_with_all_evidence"] += retained == len(c["evidence"])
        g["mean_context_bytes"] += row["serialized_context_bytes"]
    for name, actual in groups.items():
        actual["mean_context_bytes"] /= actual["questions"]
        for key, value in actual.items():
            assert abs(value - control["groups"][name][key]) < 1e-8, (arm, name, key)
    packets += len(rows)
print("Exact-range evidence audit passed; packets:", packets)
