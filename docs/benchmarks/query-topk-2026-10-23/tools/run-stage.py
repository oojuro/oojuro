import hashlib,json,os,subprocess,sys,time
from pathlib import Path
R=Path(__file__).resolve().parent
D=R.parent/'local-efficiency-20261023a/inputs'
BASE=R/'oojuro-bench-baseline'
STAGE=sys.argv[1];OUT=R/STAGE;OUT.mkdir(exist_ok=False)
arms={'baseline':BASE,'topk':R/'oojuro-bench-topk'}
spec={'scope':'same-machine warm-cache local diagnostic; exact returned-work gate; no hosted/SOTA/cost claim','stage':STAGE,'runs':3,'binary_sha256':{k:hashlib.sha256(v.read_bytes()).hexdigest() for k,v in arms.items()},'frozen_input_spec_sha256':hashlib.sha256((D.parent/'spec.json').read_bytes()).hexdigest(),'arms':{k:str(v) for k,v in arms.items()},'memory_max':'3G','swap_max':0}
(OUT/'spec.json').write_text(json.dumps(spec,indent=2)+'\n')
env=dict(os.environ,OOJURO_BENCH_DIR=str(D/'bench'),OOJURO_BENCH_LOCK=str(R.parent/'local-efficiency-bench.lock'))
def job(label,options,selected):
 command=[str(BASE),*options,'--runs','3','--mem-max','3G','--keep','--data',str(D/'data'),'--work',str(OUT/'stores'/label),'--out',str(OUT/(label+'.json'))]
 for name in selected: command += ['--arm',f'{name}=cmd:{arms[name]} serve --arm native-record']
 (OUT/(label+'-host-before.json')).write_text(json.dumps({'load':Path('/proc/loadavg').read_text(),'cpu':Path('/proc/stat').read_text(),'memory':Path('/proc/meminfo').read_text()}))
 start=time.monotonic()
 with (OUT/(label+'.log')).open('w') as log: p=subprocess.run(command,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=300)
 assert p.returncode==0
 c=json.loads((OUT/(label+'.json')).read_text());assert all(not a['error'] and a['gates'] and all(g['pass'] for g in a['gates'].values()) for a in c['arms'])
 print(label,'passed',time.monotonic()-start,flush=True)
if STAGE=='reverse':
 job('beir',['beir','--dataset','beir-arguana','--split','dev','--limit-queries','15','--k','100','--batch','100'],['topk','baseline'])
 sys.exit(0)
job('gates',['gates'],list(arms))
job('beir',['beir','--dataset','beir-arguana','--split','dev','--limit-queries','15','--k','100','--batch','100'],list(arms))
job('history',['turnloop','--set','swe-agent','--split','dev','--trajectories','2','--background','1000','--k','20','--batch','100'],['topk','baseline'])
