import hashlib,json,os,shutil,subprocess,sys,time
from pathlib import Path
HERE=Path(__file__).resolve().parent
BUNDLE=HERE.parent/'local-efficiency-20261023a'
BASE=HERE.parent/'native-profile/bin/oojuro-bench-6cec206'
def run(binary,label,profile=False):
 source=BUNDLE/'stores/beir/native-record/run0-beir/store';store=HERE/(label+'-store');assert not store.exists();shutil.copytree(source,store)
 queries={d['_id']:d['text'] for d in map(json.loads,(BUNDLE/'inputs/data/beir-arguana/arguana/queries.jsonl').read_text().splitlines())}
 ids=json.loads((BUNDLE/'fts-ranked-scores.json').read_text())['query_ids']
 env=os.environ.copy()
 if profile:env['LD_PRELOAD']='/usr/lib/x86_64-linux-gnu/libmemusage.so'
 with (HERE/(label+'.stderr')).open('w') as log:
  p=subprocess.Popen([str(binary),'serve','--arm','native-record'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,bufsize=1,env=env)
  def memory():return {k:int(v.split()[0])*1024 for line in Path(f'/proc/{p.pid}/status').read_text().splitlines() if ':' in line for k,v in [line.split(':',1)] if k in ['VmRSS','VmHWM','RssAnon']}
  def call(r):
   p.stdin.write(json.dumps(r)+'\n');p.stdin.flush();out=json.loads(p.stdout.readline());assert out['ok'],out;return out
  call({'op':'hello'});samples=[dict(phase='initial',**memory())];call({'op':'open','dir':str(store)});samples.append(dict(phase='after_open',**memory()));signatures=[]
  for n,q in enumerate(ids):
   reply=call({'op':'search','query':dict(text=queries[q],valid_at=1,known_at=1,k=101)})
   signatures.append(hashlib.sha256(json.dumps(reply['hits'],sort_keys=True,separators=(',',':')).encode()).hexdigest())
   samples.append(dict(phase='query_'+str(n),**memory()))
  call({'op':'close'});samples.append(dict(phase='after_close',**memory()));p.stdin.close();assert p.wait(timeout=10)==0
 result=dict(binary_sha256=hashlib.sha256(Path(binary).read_bytes()).hexdigest(),label=label,memory_profiled=profile,samples=samples,ranked_id_scores_sha256=signatures,scope='copied immutable fixture, phase/RSS diagnostic, not latency benchmark')
 (HERE/(label+'.json')).write_text(json.dumps(result,indent=2)+'\n');print(label,samples[1],samples[-2],samples[-1])
if __name__=='__main__':run(Path(sys.argv[1]).resolve() if len(sys.argv)>1 else BASE,sys.argv[2] if len(sys.argv)>2 else 'baseline-phase',len(sys.argv)>3)
