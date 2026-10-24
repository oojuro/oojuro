import os, pathlib, subprocess, tempfile, time, json, re, hashlib, signal, shutil
art=pathlib.Path(__file__).resolve().parent
base=art.parent
repo=base/'oojuro-main-native-dependency-policy'
old=base/'oojuro-main-native-engine-foundation'
source='5e150c97308dd38d1aa169ff8269942cea97947b'
tools=base/'public-ci-rustup/toolchains/1.99.1-x86_64-unknown-linux-gnu/bin'
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=repo,text=True).strip()==source
assert not subprocess.check_output(['git','status','--porcelain'],cwd=repo,text=True)
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=old,text=True).strip()=='ba144d90d396b680122f1e93c808c724844ef2c8'
scratch=pathlib.Path(tempfile.mkdtemp(prefix='ojdp-',dir='/var/tmp'));scratch.chmod(0o700)
assert scratch.resolve()==scratch
sha=lambda p:hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest()
env=os.environ.copy()
for key in ['RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER','RUSTC_BOOTSTRAP']:
 env.pop(key,None)
env.update(PATH=f'{tools}:{base/"portable-key-build"}:/home/rocket2/.local/bin:/home/rocket2/.cargo/bin:'+env['PATH'],RUSTUP_HOME=str(base/'public-ci-rustup'),RUSTUP_TOOLCHAIN='1.99.1',RUSTC=str(tools/'rustc'),RUSTDOC=str(tools/'rustdoc'),CARGO_TARGET_DIR=str(base/'public-ci-target'),CARGO_BUILD_JOBS='2',CARGO_TERM_COLOR='never',CARGO_NET_OFFLINE='true',PYTHONDONTWRITEBYTECODE='1',TMPDIR=str(scratch),CC=str(base/'portable-key-build/cc'),CXX=str(base/'portable-key-build/c++'))
receipt={'source':source,'base':subprocess.check_output(['git','rev-parse','HEAD'],cwd=old,text=True).strip(),'scratch':str(scratch),'toolchain':{},'gates':[],'cold_builds':[],'provenance':{'task_local_compiler':True,'global_rustup_which_auto_install':'Before any build, rustup which unexpectedly installed missing global1.99.1. It was terminal; default stable unchanged. Both actual build arms use existing task-local1.99.1 with matching rustc binary SHA, not global installation.','functional_cache':'warm public-ci-target; not used as cold measurement','cold_caches':'fresh separate Cargo targets and Zig native artifact caches; existing Cargo source/index and OS page cache not flushed','functional_env':{k:env[k] for k in ['PATH','RUSTUP_HOME','RUSTUP_TOOLCHAIN','RUSTC','RUSTDOC','CARGO_TARGET_DIR','CARGO_BUILD_JOBS','CC','CXX']}}}
for name,args in [('rustc',['-Vv']),('cargo',['-Vv'])]:
 receipt['toolchain'][name]={'path':str(tools/name),'sha256':sha(tools/name),'version':subprocess.check_output([str(tools/name),*args],env=env,text=True)}
for name in ['cc','c++']:
 receipt['toolchain'][name]={'functional':str(base/'portable-key-build'/name),'functional_sha256':sha(base/'portable-key-build'/name),'cold':str(art/name),'cold_sha256':sha(art/name)}
receipt['toolchain']['zig_version']=subprocess.check_output(['/home/rocket2/.sa-agents/oojuro-toolchain/zig/zig','version'],text=True).strip()
def save(): (art/'results.json').write_text(json.dumps(receipt,indent=2)+'\n')
def run(name,cmd,*,cwd=repo,e=env,timeout=600):
 print('START',name,flush=True);started=time.monotonic();log=art/f'{name}.log';timed_out=False
 with log.open('wb') as out:
  child=subprocess.Popen(cmd,cwd=cwd,env=e,stdout=out,stderr=subprocess.STDOUT,start_new_session=True)
  try: rc=child.wait(timeout=timeout)
  except subprocess.TimeoutExpired:
   timed_out=True;os.killpg(child.pid,signal.SIGTERM)
   try: child.wait(timeout=5)
   except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
   rc=124
 text=log.read_text(errors='replace');counts=[tuple(map(int,m)) for m in re.findall(r'test result: .*? (\d+) passed; (\d+) failed; (\d+) ignored;',text)]
 result={'name':name,'command':cmd,'cwd':str(cwd),'exit':rc,'timed_out':timed_out,'wall_seconds':round(time.monotonic()-started,3),'log_sha256':sha(log)}
 if counts: result.update(passed=sum(c[0] for c in counts),failed=sum(c[1] for c in counts),ignored=sum(c[2] for c in counts),test_binaries=len(counts))
 py=re.search(r'Ran (\d+) tests?',text)
 if py:result.update(passed=int(py[1]) if rc==0 else None)
 receipt['gates'].append(result);save();print(json.dumps(result),flush=True)
 if rc:raise SystemExit(rc)
 return result
cargo=str(tools/'cargo')
common=[cargo,'test','--offline','--locked','--no-default-features']
try:
 run('policy',['python3','scripts/dependency_audit.py','--engine-only','--check','--output',str(art/'minimal.json')],timeout=120)
 run('policy-negative',['python3','scripts/test_dependency_audit.py'],timeout=60)
 run('storage',common+['--lib','--tests','-p','oojuro-core','-p','oojuro-store','-p','oojuro-nine'])
 run('native',common+['--lib','--tests','-p','oojuro-engine','-p','oojuro-rank','--','--test-threads=1'])
 run('compat',common+['-p','oojuro-trace','--test','compat'],timeout=180)
 run('bench-tools',[cargo,'check','--offline','--locked','-p','oojuro-store','--features','bench-tools','--bin','store-bench','--example','frame_probe'],timeout=360)
 run('packaging',['python3','scripts/test_package_native.py'],timeout=60)
 # Freeze the actual engine unit-test binary before later builds can replace it.
 bins=[p for p in (base/'public-ci-target/debug/deps').glob('oojuro_engine-*') if p.is_file() and os.access(p,os.X_OK) and p.read_bytes()[:4]==b'\x7fELF']
 newest=max(bins,key=lambda p:p.stat().st_mtime_ns);frozen=art/'oojuro-engine-qualified-tests';shutil.copyfile(newest,frozen);frozen.chmod(0o500)
 receipt['qualified_test_binary']={'path':str(frozen),'sha256':sha(frozen),'source':source,'original':str(newest)};save()
 for arm,cwd in [('before',old),('after',repo)]:
  target=art/f'cold-{arm}-target';cache=art/f'cold-{arm}-zig-cache'
  assert not target.exists() and not cache.exists()
  target.mkdir();cache.mkdir()
  e=env.copy();e.update(CARGO_TARGET_DIR=str(target),OOJURO_DEP_ZIG_CACHE=str(cache),CC=str(art/'cc'),CXX=str(art/'c++'),RUSTFLAGS='-C target-cpu=x86-64')
  timing=art/f'cold-{arm}.time'
  result=run('cold-'+arm,['/usr/bin/time','-v','-o',str(timing),cargo,'build','--offline','--locked','--no-default-features','--release','--lib','-p','oojuro-engine','--target','x86_64-unknown-linux-gnu'],cwd=cwd,e=e,timeout=480)
  raw=timing.read_text();rss=re.search(r'Maximum resident set size \(kbytes\): (\d+)',raw)
  library=target/'x86_64-unknown-linux-gnu/release/liboojuro_engine.rlib'
  compiled=sorted(set(re.findall(r'^\s*Compiling (\S+) v(\S+)',(art/f'cold-{arm}.log').read_text(),re.M)))
  native=[{'path':str(p.relative_to(target)),'bytes':p.stat().st_size,'sha256':sha(p)} for p in sorted(target.rglob('*.a'))]
  receipt['cold_builds'].append({'arm':arm,'source':subprocess.check_output(['git','rev-parse','HEAD'],cwd=cwd,text=True).strip(),'target':str(target),'zig_cache':str(cache),'initially_empty':True,'rustflags':e['RUSTFLAGS'],'cc':e['CC'],'cxx':e['CXX'],'wall_seconds':result['wall_seconds'],'maximum_single_process_rss_kib':int(rss[1]) if rss else None,'time_sha256':sha(timing),'compiled_package_name_versions':compiled,'library':{'path':str(library),'bytes':library.stat().st_size,'sha256':sha(library)},'native_archives':native});save()
 receipt['status']='all functional gates and one bounded matched fresh-artifact build pair passed';save()
finally:
 receipt['scratch_entries_retained']=[str(p.relative_to(scratch)) for p in scratch.iterdir()]
 # This uniquely created scratch contains only synthetic fixtures from this run.
 shutil.rmtree(scratch);receipt['scratch_removed']=not scratch.exists();save()
