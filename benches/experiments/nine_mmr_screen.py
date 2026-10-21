"""Frozen-window MMR diagnostic; never prints private queries, text or IDs."""
import argparse
import json
import re
import sqlite3
from collections import defaultdict
from pathlib import Path
from time import perf_counter

parser = argparse.ArgumentParser()
parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[2])
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
root = args.repo_root.resolve()
lib = (root / "crates/oojuro-nine/src/lib.rs").read_text(encoding="utf-8")
section = lib.split("pub const QUERY_STOP_WORDS:", 1)[1].split("];", 1)[0]
stop = set(re.findall(r'"([a-z]+)"', section))
assert {"the", "a", "and", "what", "which", "for"} <= stop
cases = json.loads((root / "data/private/nine-combined-cases-v1.json").read_text(encoding="utf-8"))
db = sqlite3.connect(f"file:{(root / 'data/private/nine-scale-run3/sources.sqlite').as_posix()}?mode=ro", uri=True)
out = args.output if args.output.is_absolute() else root / args.output
assert not out.exists()
arms = {}

def encoded(snippets):
    return json.dumps(snippets, ensure_ascii=False, separators=(",", ":")).encode()

def full_pair_set(c, snippets):
    found = defaultdict(list)
    for e in c["evidence"]:
        found[e["source_id"]].append(e["quote"])
    selected = {s["id"]: s["text"] for s in snippets}
    return {sid for sid, quotes in found.items() if sid in selected and all(q in selected[sid] for q in quotes)}

def load_snippets(row):
    snippets = []
    for sid, (a, b) in row["source_ranges"]:
        key, at, known, verified, text = db.execute("select source_key,valid_at,known_at,verified,substr(cast(body as blob),?1+1,?2-?1) from sources where id=?3", (a,b,sid)).fetchone()
        snippets.append(dict(id=sid, key=key, text=text.decode(), valid_at=at,
                             recorded_at=known, verified=bool(verified), source_range=[a,b]))
    assert len(encoded(snippets)) == row["serialized_context_bytes"]
    assert [s["id"] for s in snippets] == row["context_ids"]
    return snippets

def select(snippets, ranks, diverse):
    features = [{w for w in re.findall(r"[^\W_]+", s["text"].lower()) if w not in stop} for s in snippets]
    costs = [len(encoded([s]))-2 for s in snippets]
    selected, pending = [], set(range(len(snippets)))
    similarity = [0.] * len(snippets)
    size = 2
    while pending and len(selected) < 32:
        feasible = [i for i in pending if size + costs[i] + bool(selected) <= 65536]
        if not feasible:
            break
        if diverse:
            i = max(feasible, key=lambda j: (.5 / ranks[j] - .5 * similarity[j], -ranks[j]))
        else:
            i = min(feasible, key=lambda j: ranks[j])
        selected.append(i)
        pending.remove(i)
        size += costs[i] + (len(selected) > 1)
        if diverse:
            for j in pending:
                intersection = len(features[i] & features[j])
                union = len(features[i]) + len(features[j]) - intersection
                similarity[j] = max(similarity[j], intersection / union if union else 0.)
    selected.sort(key=lambda j: ranks[j])
    packet = [snippets[i] for i in selected]
    assert len(encoded(packet)) == size <= 65536
    return packet

for engine, source in [("nine", "nine-context-frontier1"), ("sqlite_fts5", "nine-context-frontier-rust1")]:
    wide = json.loads((root / f"data/private/{source}/{engine}_k512_b1048576.json").read_text(encoding="utf-8"))
    base = json.loads((root / f"data/private/{source}/{engine}_k32_b65536.json").read_text(encoding="utf-8"))
    groups = defaultdict(lambda: defaultdict(list))
    for ordinal, (labeled, row, previous) in enumerate(zip(cases, wide, base)):
        c, group = labeled["case"], labeled["group"]
        assert row["case_id"] == previous["case_id"] == c["id"]
        snippets = load_snippets(row)
        rank_map = {sid: i+1 for i, sid in enumerate(row["candidate_ids"])}
        ranks = [rank_map[s["id"]] for s in snippets]
        prior = load_snippets(previous)
        prior_refs = full_pair_set(c, prior)
        ordered = select(snippets, ranks, False)
        ordered_refs = full_pair_set(c, ordered)
        skip = '"' in c["question"] or "_" in c["question"] or c["question"].startswith(("Find the implementation of the Rust function ", "Locate the Rust declaration of "))
        start = perf_counter()
        chosen = ordered if skip else select(snippets, ranks, True)
        elapsed = (perf_counter()-start)*1000
        found = full_pair_set(c, chosen)
        g = groups[group]
        for k, v in dict(prior_pairs=len(prior_refs), ordered_pairs=len(ordered_refs), selected_pairs=len(found),
                         gained_pairs=len(found-prior_refs), lost_pairs=len(prior_refs-found),
                         prior_bytes=len(encoded(prior)), selected_bytes=len(encoded(chosen)),
                         prior_complete=int(len(prior_refs)==len({e['source_id'] for e in c['evidence']})),
                         selected_complete=int(len(found)==len({e['source_id'] for e in c['evidence']})),
                         available_windows=len(snippets), unavailable_windows=len(row['candidate_ids'])-len(snippets),
                         selection_only_ms=elapsed).items():
            g[k].append(v)
        if found-prior_refs: g['gained_case_ordinals'].append(ordinal)
        if prior_refs-found: g['lost_case_ordinals'].append(ordinal)
    arms[engine] = {}
    for group, fields in groups.items():
        result = {}
        for k, values in fields.items():
            result[k] = values if k.endswith('_ordinals') else (sum(values)/len(values) if k.endswith('_bytes') or k.endswith('_ms') or k.endswith('_windows') else sum(values))
        arms[engine][group] = result
        print(engine, group, 'references', result['prior_pairs'], '->', result['selected_pairs'],
              'new', result['gained_pairs'], 'lost', result['lost_pairs'],
              'screen-only mean ms', round(result['selection_only_ms'], 2))
result = dict(scope="frozen-window offline complementary-context screen", coefficient=.5,
              max_sources=32, max_context_bytes=65536, embedding_calls=0, llm_calls=0, arms=arms,
              limitations=["Retrospective development fixtures; labels are used only for scoring after selection.",
                           "Frozen wide-packet windows can omit late candidates or differ in final clipping; not fresh extraction.",
                           "Timing is Python selection only, excludes original loading, token preparation for the ordered control, retrieval and receipts.",
                           "Lexical MMR and reciprocal original rank; no entropy or factual-confidence guarantee."])
out.write_text(json.dumps(result,indent=2)+'\n', encoding="utf-8")
print('Aggregate screen saved; no query or source content printed.')
