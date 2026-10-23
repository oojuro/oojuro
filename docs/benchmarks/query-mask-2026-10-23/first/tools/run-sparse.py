import hashlib,json,os,subprocess,uuid
from pathlib import Path
R=Path(__file__).resolve().parent;OUT=R/'sparse-first';OUT.mkdir(exist_ok=False);cpu=2 if 2 in os.sched_getaffinity(0) else min(os.sched_getaffinity(0));results=[]
for label,n in [('baseline',0),('mask',0),('mask',1),('baseline',1),('baseline',2),('mask',2)]:
 binary=R/('sparse-'+label+'-test');log=OUT/(label+'-'+str(n)+'.log')
 command=['systemd-run','--user','--scope','--quiet','--collect','--unit=oojuro-sparse-'+uuid.uuid4().hex,'--property=MemoryMax=3G','--property=MemorySwapMax=0','--','taskset','-c',str(cpu),str(binary),'--exact','lexical::tests::sparse_query_probe','--ignored','--nocapture','--test-threads=1']
 with log.open('w') as f:p=subprocess.run(command,stdout=f,stderr=subprocess.STDOUT,timeout=30)
 assert p.returncode==0
 rows=[]
 for line in log.read_text().splitlines():
  start=line.find('{')
  if start>=0:
   row=json.loads(line[start:])
   if row.get('diagnostic')=='sparse-query':rows.append(row)
 assert len(rows)==3 and all(len(q['samples'])==2000 for q in rows)
 results.append(dict(arm=label,run=n,queries=rows,binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest()));print(label,n,[(q['label'],q['p50_ns']) for q in rows],flush=True)
for r in results:assert [(q['label'],q['signature']) for q in r['queries']]==[(q['label'],q['signature']) for q in results[0]['queries']]
(OUT/'matrix.json').write_text(json.dumps(dict(cpu_affinity=cpu,rows=100000,warmup_per_case=100,samples_per_case=2000,scope='bounded sparse component diagnostic only; public/full-pipeline comparisons remain separate',diagnostic_sha256=hashlib.sha256((R/'sparse-probe.rs').read_bytes()).hexdigest(),host_load=Path('/proc/loadavg').read_text(),results=results),indent=2)+'\n')
