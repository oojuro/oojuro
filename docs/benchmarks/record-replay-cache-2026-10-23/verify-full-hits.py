import hashlib,json,shutil,subprocess,sys
from pathlib import Path
R=Path(__file__).resolve().parent;D=R.parent/'local-efficiency-20261023a';label=sys.argv[1]
queries={d['_id']:d['text'] for d in map(json.loads,(D/'inputs/data/beir-arguana/arguana/queries.jsonl').read_text().splitlines())};ids=json.loads((D/'fts-ranked-scores.json').read_text())['query_ids'];q=R/'queries15.json'
if not q.exists():q.write_text(json.dumps([dict(id=i,text=queries[i]) for i in ids])+'\n')
binary={'baseline':R/'profile-native-baseline','cleared':R/'profile-native-cleared'}[label]
store=R/(label+'-full-hits-store');assert not store.exists();shutil.copytree(D/'stores/beir/native-record/run0-beir/store',store)
with (R/(label+'-full-hits.jsonl')).open('w') as out,(R/(label+'-full-hits.log')).open('w') as log:subprocess.run([str(binary),str(store),str(q)],stdout=out,stderr=log,check=True,timeout=180)
rows=[json.loads(l) for l in (R/(label+'-full-hits.jsonl')).read_text().splitlines()];assert len(rows)==45 and all(x['exact_ranked_ids_and_score_bits'] and x['originals_exact'] and x['records']==8674 for x in rows)
if label!='baseline':
 base=[json.loads(l) for l in (R/'baseline-full-hits.jsonl').read_text().splitlines()]
 assert [(x['run'],x['query_blake3'],x['ranked_ids_bits_metadata_excerpt_blake3']) for x in rows]==[(x['run'],x['query_blake3'],x['ranked_ids_bits_metadata_excerpt_blake3']) for x in base]
print(label,'45exact full ranked IDs/score bits/clocks/metadata/excerpts +original checks passed')
