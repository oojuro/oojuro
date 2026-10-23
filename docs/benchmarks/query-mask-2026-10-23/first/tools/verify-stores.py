import hashlib,json,subprocess,uuid
from pathlib import Path
R=Path(__file__).resolve().parent;D=R.parent/'local-efficiency-20261023a';root=R/'matrix'
bins={'baseline':R/'oojuro-bench-baseline','mask':R/'oojuro-bench-mask'}
canonical=lambda rows:json.dumps(sorted([{k:d.get(k,{} if k=='meta' else '') for k in ['id','key','text','valid_at','recorded_at','meta']} for d in rows],key=lambda d:d['id']),ensure_ascii=False,sort_keys=True,separators=(',',':')).encode()
expected=[]
for line in (D/'inputs/data/beir-arguana/arguana/corpus.jsonl').read_text().splitlines():
 d=json.loads(line);expected.append(dict(id=d['_id'],key='',text=(d.get('title','')+'\n' if d.get('title') else '')+d['text'],valid_at=0,recorded_at=0,meta={}))
expected=canonical(expected);checks=[];files=[];reference=None
for suite,session in [('beir','beir'),('history','turnloop')]:
 card=json.loads((root/(suite+'.json')).read_text())
 for name,binary in bins.items():
  arm=next(a for a in card['arms'] if a['label']==name)
  for run in range(3):
   directory=root/'stores'/suite/name/f'run{run}-{session}'/'store'
   requests=[dict(op='open',dir=str(directory)),{'op':'get_range','from':-9223372036854775808,'to':9223372036854775807,'limit':100000},{'op':'close'}]
   command=['systemd-run','--user','--scope','--quiet','--collect','--unit=oojuro-ram-reopen-'+uuid.uuid4().hex,'--property=MemoryMax=3G','--property=MemorySwapMax=0','--',str(binary),'serve','--arm','native-record']
   p=subprocess.run(command,input=''.join(json.dumps(r)+'\n' for r in requests),capture_output=True,text=True,check=True,timeout=30)
   responses=[json.loads(line) for line in p.stdout.splitlines()];assert len(responses)==3 and all(r['ok'] for r in responses)
   rows=responses[1]['docs'];encoded=canonical(rows)
   if suite=='beir':assert encoded==expected
   else:
    if reference is None:reference=encoded
    assert encoded==reference
   inventory=[]
   for path in sorted(directory.rglob('*')):
    if path.is_file():
     stat=path.stat();inventory.append(dict(suite=suite,regime=name,run=run,path=str(path.relative_to(directory)),apparent=stat.st_size,allocated=stat.st_blocks*512,sha256=hashlib.sha256(path.read_bytes()).hexdigest()))
   assert sum(x['apparent'] for x in inventory)==arm['storage']['disk_bytes']['runs'][run]
   assert sum(x['allocated'] for x in inventory)==arm['storage']['disk_alloc_bytes']['runs'][run]
   files.extend(inventory);checks.append(dict(suite=suite,regime=name,run=run,records=len(rows),all_fields_exact=True,canonical_sha256=hashlib.sha256(encoded).hexdigest(),footprint_matches=True))
(root/'reopen-and-files.json').write_text(json.dumps(dict(checks=checks,files=files),indent=2)+'\n');print('12 fresh store reopens/all fields exact;',len(files),'files;24footprints match')
