"""Local frozen ArguAna native comparison; no AWS/hosted acceptance claim."""
import hashlib,json,math,os,statistics,subprocess,time,sys
from pathlib import Path
ROOT=Path(sys.argv[1]).resolve()
HERE=ROOT/'work/native-profile'
CORPUS=ROOT/'work/native-benchmark/data/beir-arguana/arguana/corpus.jsonl'
DOCS=[]
for line in CORPUS.read_text().split('\n'):
    if not line: continue
    r=json.loads(line)
    DOCS.append(dict(id=r['_id'],text=(r.get('title','')+'\n' if r.get('title') else '')+r['text'],valid_at=0,recorded_at=0))
assert len(DOCS)==8674 and len({d['id'] for d in DOCS})==len(DOCS)
BYID={d['id']:d['text'] for d in DOCS}
QUERIES=json.loads((HERE/'queries.json').read_text())

def summary(samples):
    a=sorted(samples)
    return dict(n=len(a),p50_us=statistics.median(a),p99_us=a[math.ceil(len(a)*.99)-1],max_us=a[-1],tail_note='nearest-rank empirical percentile; small local sample, not a production tail estimate')

class Arm:
    def __init__(self,label,directory):
        self.p=subprocess.Popen([str(HERE/'bin'/f'oojuro-bench-{label}'),'serve','--arm','native-record'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,bufsize=1)
        self.info=self.call(dict(op='hello'))[0]['info']
        self.call(dict(op='open',dir=str(directory)))
    def call(self,request):
        line=json.dumps(request,ensure_ascii=False,separators=(',',':'))+'\n'
        start=time.perf_counter_ns()
        self.p.stdin.write(line);self.p.stdin.flush()
        r=json.loads(self.p.stdout.readline())
        us=(time.perf_counter_ns()-start)/1000
        assert r['ok'],r
        return r,us
    def memory(self):
        return {line.split(':')[0]:line.split(':')[1].strip() for line in Path(f'/proc/{self.p.pid}/status').read_text().splitlines() if line.startswith(('VmHWM:','VmRSS:'))}
    def close(self):
        self.call(dict(op='close'));self.p.stdin.close()
        assert self.p.wait()==0,self.p.stderr.read()
        self.p.stdout.close();self.p.stderr.close()

results=[]
for run in range(3):
    for label in (['14c7881','6cec206'] if run%2==0 else ['6cec206','14c7881']):
        directory=HERE/'single-text-final-stores'/label/f'run{run}'
        assert not directory.exists(),f'refuse to overwrite {directory}'
        arm=Arm(label,directory)
        append=[];append_roundtrip=[];search=[];get=[];endtoend=[];signatures=[]
        for offset in range(0,len(DOCS),100):
            r,us=arm.call(dict(op='append_batch',docs=DOCS[offset:offset+100]))
            append.append(r['elapsed_us']);append_roundtrip.append(us)
        memory_before_search=arm.memory()
        memory_after_cold=None
        for repetition in range(3):
            for q in QUERIES:
                r,search_us=arm.call(dict(op='search',query=dict(text=q['text'],valid_at=9223372036854775807,known_at=9223372036854775807,k=101)))
                ids=[h['id'] for h in r.get('hits',[]) if h['id']!=q['id']][:100]
                originals,get_us=arm.call(dict(op='get',ids=ids))
                assert originals['texts']==[BYID[i] for i in ids]
                search.append(dict(service_us=r['elapsed_us'],roundtrip_us=search_us))
                get.append(dict(service_us=originals['elapsed_us'],roundtrip_us=get_us))
                endtoend.append(search_us+get_us)
                if memory_after_cold is None: memory_after_cold=arm.memory()
                signatures.append(hashlib.sha256(json.dumps(r['hits'],sort_keys=True,separators=(',',':')).encode()).hexdigest())
        memory=arm.memory();arm.close()
        files=[dict(path=str(p.relative_to(directory)),apparent=p.stat().st_size,allocated=p.stat().st_blocks*512) for p in sorted(directory.rglob('*')) if p.is_file()]
        reopened=Arm(label,directory)
        original,_=reopened.call(dict(op='get_range',**{'from':-9223372036854775808,'to':9223372036854775807,'limit':10000}))
        expected=DOCS
        normalized=lambda d:dict(id=d['id'],key=d.get('key',''),text=d['text'],valid_at=d['valid_at'],recorded_at=d['recorded_at'],meta=d.get('meta',{}))
        assert [normalized(d) for d in original['docs']]==[normalized(d) for d in expected]
        reopened.close()
        result=dict(label=label,run=run,info=arm.info,append_service_us=append,append_roundtrip_us=append_roundtrip,search=search,get=get,endtoend_roundtrip_us=endtoend,ranked_ids_scores_sha256=signatures,memory=memory,memory_before_search=memory_before_search,memory_after_cold=memory_after_cold,store_files=files,store_apparent_bytes=sum(f['apparent'] for f in files),store_allocated_bytes=sum(f['allocated'] for f in files),all_8674_reopened_documents_exact=True)
        results.append(result)
        (HERE/'single-text-final-io-inprogress.json').write_text(json.dumps(results,indent=2)+'\n')
        print(f'finished run{run} {label}: search+get p50 {statistics.median(endtoend):.0f}us; store {result["store_apparent_bytes"]}',flush=True)
for run in range(3):
    a,b=[r for r in results if r['run']==run]
    assert a['ranked_ids_scores_sha256']==b['ranked_ids_scores_sha256']
report=dict(scope='local same-machine diagnostic; native fsync single host, warm OS cache; no hosted/AWS/SOTA or pricing claim',records=len(DOCS),text_bytes=sum(len(d['text'].encode()) for d in DOCS),batch_size=100,corpus_sha256=hashlib.sha256(CORPUS.read_bytes()).hexdigest(),runs=results,summaries={})
for label in ['14c7881','6cec206']:
    rows=[r for r in results if r['label']==label]
    values=lambda key:[x for r in rows for x in r[key]]
    report['summaries'][label]=dict(append_service=summary(values('append_service_us')),append_roundtrip=summary(values('append_roundtrip_us')),records_per_append_service_second=len(DOCS)*3/(sum(values('append_service_us'))/1e6),cold_search_plus_get=summary([r['endtoend_roundtrip_us'][0] for r in rows]),reused_search_plus_get=summary([x for r in rows for x in r['endtoend_roundtrip_us'][1:]]),get_service=summary([x['service_us'] for r in rows for x in r['get']]),store_apparent_bytes=[r['store_apparent_bytes'] for r in rows],store_allocated_bytes=[r['store_allocated_bytes'] for r in rows],memory=[r['memory'] for r in rows])
(HERE/'single-text-final-io-profile.json').write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps(report['summaries'],indent=2),flush=True)
