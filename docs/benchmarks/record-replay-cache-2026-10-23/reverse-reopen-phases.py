# Extends the existing copied-store memory-phases diagnostic with matched originals/timings.
import hashlib,json,shutil,subprocess,time,uuid
from pathlib import Path
ROOT=Path(__file__).resolve().parent;R=ROOT/'reverse';R.mkdir(exist_ok=False);D=ROOT.parent/'local-efficiency-20261023a';SOURCE=D/'stores/beir/native-record/run0-beir/store'
queries={d['_id']:d['text'] for d in map(json.loads,(D/'inputs/data/beir-arguana/arguana/queries.jsonl').read_text().splitlines())};query_ids=json.loads((D/'fts-ranked-scores.json').read_text())['query_ids']
originals={d['_id']:(d.get('title','')+'\n' if d.get('title') else '')+d['text'] for d in map(json.loads,(D/'inputs/data/beir-arguana/arguana/corpus.jsonl').read_text().splitlines())}
def inventory(store):
 return [dict(path=str(p.relative_to(store)),size=p.stat().st_size,allocated=p.stat().st_blocks*512,sha256=hashlib.sha256(p.read_bytes()).hexdigest()) for p in sorted(store.rglob('*')) if p.is_file()]
def run(label,n):
 binary=ROOT/('oojuro-bench-'+label);store=R/(label+'-phase-'+str(n)+'-store');assert not store.exists();shutil.copytree(SOURCE,store);before=inventory(store)
 with (R/(label+'-phase-'+str(n)+'.log')).open('w') as log:
  command=['systemd-run','--user','--scope','--quiet','--collect','--unit=oojuro-cache-phase-'+uuid.uuid4().hex,'--property=MemoryMax=3G','--property=MemorySwapMax=0','--',str(binary),'serve','--arm','native-record']
  p=subprocess.Popen(command,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,bufsize=1)
  def memory():return {k:int(v.split()[0])*1024 for line in Path(f'/proc/{p.pid}/status').read_text().splitlines() if ':' in line for k,v in [line.split(':',1)] if k in ['VmRSS','VmHWM','RssAnon']}
  def call(request):
   start=time.monotonic_ns();p.stdin.write(json.dumps(request)+'\n');p.stdin.flush();out=json.loads(p.stdout.readline());assert out['ok'],out;return out,(time.monotonic_ns()-start)/1000
  call({'op':'hello'});assert Path(f'/proc/{p.pid}/exe').resolve()==binary.resolve();samples=[dict(phase='initial',**memory())]
  opened,open_us=call({'op':'open','dir':str(store)});samples.append(dict(phase='after_open',**memory()));results=[]
  for q in query_ids:
   reply,search_roundtrip=call({'op':'search','query':dict(text=queries[q],valid_at=1,known_at=1,k=101)})
   ids=[h['id'] for h in reply['hits'] if h['id']!=q][:100];texts,get_roundtrip=call({'op':'get','ids':ids});assert texts['texts']==[originals[i] for i in ids]
   results.append(dict(query_id=q,ranked_ids_score_sha256=hashlib.sha256(json.dumps(reply['hits'],sort_keys=True,separators=(',',':')).encode()).hexdigest(),originals_exact=True,returned_original_bytes=sum(len(t.encode()) for t in texts['texts']),search_us=reply['elapsed_us'],get_us=texts['elapsed_us'],search_get_roundtrip_us=search_roundtrip+get_roundtrip))
   samples.append(dict(phase='query_'+str(len(results)-1),**memory()))
  call({'op':'close'});samples.append(dict(phase='after_close',**memory()));p.stdin.close();assert p.wait(timeout=10)==0
 assert inventory(store)==before
 result=dict(label=label,run=n,binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),open_us=opened['elapsed_us'],open_roundtrip_us=open_us,samples=samples,results=results,store_bytes_unchanged=True,store_files=before,host_load=Path('/proc/loadavg').read_text(),scope='Local copied public immutable fixture; fresh process per run; warm OS cache; full search + exact original get; no hosted latency/capacity claim')
 (R/(label+'-phase-'+str(n)+'.json')).write_text(json.dumps(result,indent=2)+'\n');print(label,n,'open RSS',samples[1]['VmRSS'],'last query RSS',samples[-2]['VmRSS'],flush=True)
for label,n in [('cleared',2),('baseline',2),('baseline',1),('cleared',1),('cleared',0),('baseline',0)]:run(label,n)
all_results={label:[json.loads((R/(label+'-phase-'+str(n)+'.json')).read_text()) for n in range(3)] for label in ['baseline','cleared']}
for run in all_results['cleared']:
 assert [(r['query_id'],r['ranked_ids_score_sha256'],r['returned_original_bytes']) for r in run['results']]==[(r['query_id'],r['ranked_ids_score_sha256'],r['returned_original_bytes']) for r in all_results['baseline'][0]['results']]
(R/'phase-matrix.json').write_text(json.dumps(all_results,indent=2)+'\n')
