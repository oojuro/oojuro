import fcntl,hashlib,json,os,statistics,subprocess,time,uuid
from pathlib import Path
R=Path(__file__).resolve().parent
OUT=R/'phases';OUT.mkdir(exist_ok=False)
cpu=2 if 2 in os.sched_getaffinity(0) else min(os.sched_getaffinity(0))
results=[]
lock=(R.parent/'local-efficiency-bench.lock').open('a+')
fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
start=time.monotonic()
deadline=start+120
for run in range(3):
 for case in ['small','large','escaped']:
  for arm in (['baseline','stream'] if run%2==0 else ['stream','baseline']):
   binary=R/(arm+'-test');log=OUT/(f'{case}-{arm}-{run}.log')
   unit='oojuro-tag-'+uuid.uuid4().hex
   command=['systemd-run','--user','--scope','--quiet','--collect','--unit='+unit,'--property=MemoryMax=3G','--property=MemorySwapMax=0','--','taskset','-c',str(cpu),str(binary),'--exact','record::tests::record_append_phase_probe','--ignored','--nocapture','--test-threads=1']
   remaining=min(30,deadline-time.monotonic());assert remaining>0,'total diagnostic deadline'
   try:
    with log.open('w') as f:p=subprocess.run(command,cwd=R.parent/'oojuro-record-tag-stream',env=dict(os.environ,OOJURO_TAG_PROBE_CASE=case),stdout=f,stderr=subprocess.STDOUT,timeout=remaining)
   except subprocess.TimeoutExpired:
    subprocess.run(['systemctl','--user','stop',unit+'.scope'],capture_output=True,timeout=5)
    raise
   assert p.returncode==0,(arm,case,run,log)
   rows=[]
   for line in log.read_text().splitlines():
    at=line.find('{')
    if at>=0:
     row=json.loads(line[at:])
     if row.get('diagnostic')=='record-tag-append':rows.append(row)
   assert len(rows)==1 and rows[0]['exact_reopen']
   result=rows[0];result.update(arm=arm,run=run)
   result['phase_medians_us']={key:statistics.median(s[key] for s in result['samples']) for key in result['samples'][0]}
   results.append(result)
   (OUT/'partial.json').write_text(json.dumps(results,indent=2)+'\n')
   print(case,arm,run,result['phase_medians_us'],flush=True)
for case in ['small','large','escaped']:
 signatures={r['all_assigned_signature'] for r in results if r['case']==case};assert len(signatures)==1
manifest={'baseline_source':'c42c10d32364d692bccc97030c447dc05ceb85b0','candidate_source':'dc0e9a593ee19009126a6626a94049ffca1d9359','diagnostic_sha256':hashlib.sha256((R/'append-probe.rs').read_bytes()).hexdigest(),'binary_sha256':{arm:hashlib.sha256((R/(arm+'-test')).read_bytes()).hexdigest() for arm in ['baseline','stream']},'cpu_affinity':cpu,'elapsed_s':time.monotonic()-start,'host_load_after':Path('/proc/loadavg').read_text(),'scope':'Synthetic durable append component diagnostic only; maxes/tails with4/6 large samples are not a tail qualification; no AWS/hosted/SOTA/cost claim','results':results}
(OUT/'matrix.json').write_text(json.dumps(manifest,indent=2)+'\n')
