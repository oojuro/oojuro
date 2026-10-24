import os, pathlib, subprocess, tempfile, time, json, re, hashlib, signal, shutil
art=pathlib.Path(__file__).resolve().parent
base=art.parent
repo=base/'oojuro-combined-client-dependency-policy'
source='f726bab51d982896a129d4bdf2fd2d3a2edc2fff'
tools=base/'public-ci-rustup/toolchains/1.99.1-x86_64-unknown-linux-gnu/bin'
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=repo,text=True).strip()==source
assert not subprocess.check_output(['git','status','--porcelain'],cwd=repo,text=True)
scratch=pathlib.Path(tempfile.mkdtemp(prefix='ojcd-',dir='/var/tmp'));scratch.chmod(0o700)
assert scratch.resolve()==scratch
sha=lambda p:hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest()
env=os.environ.copy()
for key in ['RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER','RUSTC_BOOTSTRAP']:
 env.pop(key,None)
env.update(PATH=f'{tools}:{base/"portable-key-build"}:/home/rocket2/.local/bin:/home/rocket2/.cargo/bin:'+env['PATH'],RUSTUP_HOME=str(base/'public-ci-rustup'),RUSTUP_TOOLCHAIN='1.99.1',RUSTC=str(tools/'rustc'),RUSTDOC=str(tools/'rustdoc'),RUSTFLAGS='-C target-cpu=x86-64',CARGO_TARGET_DIR=str(base/'public-ci-target'),CARGO_BUILD_JOBS='2',CARGO_TERM_COLOR='never',CARGO_NET_OFFLINE='true',PYTHONDONTWRITEBYTECODE='1',TMPDIR=str(scratch),CC=str(base/'portable-key-build/cc'),CXX=str(base/'portable-key-build/c++'))
receipt={'format':1,'source':source,'repository':'oojuro/oojuro','scratch':str(scratch),'toolchain':{},'gates':[],'scope':'Combined policy and CLI consumer qualification only; unchanged SDK/TS/package/setup suites and library recovery suites not repeated. No hosted/owner/provider/deployment claim.','environment':{k:env[k] for k in ['RUSTUP_HOME','RUSTUP_TOOLCHAIN','RUSTC','RUSTDOC','RUSTFLAGS','CARGO_TARGET_DIR','CARGO_BUILD_JOBS','CC','CXX']},'profile':'debug/test; warm Cargo artifact cache; no timing or cold-build comparison','target':'x86_64-unknown-linux-gnu host target'}
for name in ['rustc','cargo']:
 receipt['toolchain'][name]={'path':str(tools/name),'sha256':sha(tools/name),'version':subprocess.check_output([str(tools/name),'-Vv'],env=env,text=True)}
for name in ['cc','c++']:
 receipt['toolchain'][name]={'path':str(base/'portable-key-build'/name),'sha256':sha(base/'portable-key-build'/name)}
def save(): (art/'results.json').write_text(json.dumps(receipt,indent=2)+'\n')
def run(name,cmd,timeout=600):
 print('START',name,flush=True);started=time.monotonic();log=art/f'{name}.log';timed_out=False
 with log.open('wb') as out:
  child=subprocess.Popen(cmd,cwd=repo,env=env,stdout=out,stderr=subprocess.STDOUT,start_new_session=True)
  try: rc=child.wait(timeout=timeout)
  except subprocess.TimeoutExpired:
   timed_out=True;os.killpg(child.pid,signal.SIGTERM)
   try: child.wait(timeout=5)
   except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
   rc=124
 text=log.read_text(errors='replace');counts=[tuple(map(int,m)) for m in re.findall(r'test result: .*? (\d+) passed; (\d+) failed; (\d+) ignored;',text)]
 result={'name':name,'command':cmd,'cwd':str(repo),'exit':rc,'timed_out':timed_out,'wall_seconds':round(time.monotonic()-started,3),'log_sha256':sha(log)}
 if counts: result.update(passed=sum(c[0] for c in counts),failed=sum(c[1] for c in counts),ignored=sum(c[2] for c in counts),test_binaries=len(counts))
 py=re.search(r'Ran (\d+) tests?',text)
 if py:result.update(passed=int(py[1]) if rc==0 else None)
 receipt['gates'].append(result);save();print(json.dumps(result),flush=True)
 if rc:raise SystemExit(rc)
 return result
cargo=str(tools/'cargo')
try:
 run('policy',['python3','scripts/dependency_audit.py','--engine-only','--check','--output',str(art/'minimal.json')],120)
 run('policy-negative',['python3','scripts/test_dependency_audit.py'],60)
 run('cli-tests',[cargo,'test','--offline','--locked','--no-default-features','--lib','--tests','-p','oojuro'],600)
 run('cli-build',[cargo,'build','--offline','--locked','--no-default-features','-p','oojuro','--bin','oojuro'],360)
 original=base/'public-ci-target/debug/oojuro';frozen=art/'oojuro-combined-cli';shutil.copyfile(original,frozen);frozen.chmod(0o500)
 receipt['cli']={'path':str(frozen),'original':str(original),'sha256':sha(frozen),'bytes':frozen.stat().st_size,'source':source,'profile':'debug','features':'--no-default-features','package':'oojuro','binary':'oojuro'}
 receipt['cli']['dynamic_links']=subprocess.check_output(['ldd',str(frozen)],text=True)
 receipt['status']='all focused combined gates passed; actual CLI frozen for separate hosted qualification';save()
finally:
 receipt['scratch_entries_retained']=[str(p.relative_to(scratch)) for p in scratch.iterdir()]
 shutil.rmtree(scratch);receipt['scratch_removed']=not scratch.exists();save()
