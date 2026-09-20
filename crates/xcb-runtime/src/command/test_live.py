#!/usr/bin/env python3
"""Credential-free adversarial proof for one explicitly selected XCB backend.
Run through hra-host-run --lane=mac-native. Does not publish guest changes.
"""
import importlib.util,sys
import argparse,base64,hashlib,json,os,pathlib,subprocess,time,uuid

def main():
    parser=argparse.ArgumentParser();parser.add_argument('--root',type=pathlib.Path,required=True);parser.add_argument('--output',type=pathlib.Path,required=True)
    parser.add_argument('--candidate-manifest',type=pathlib.Path,help='private setup candidate; never activates a backend')
    args=parser.parse_args();root=args.root
    enc=lambda v:json.dumps(v,sort_keys=True,separators=(',',':'),ensure_ascii=False).encode()
    sha=lambda b:hashlib.sha256(b).hexdigest()
    manifest_bytes=(args.candidate_manifest or root/'backend.json').read_bytes()
    backend=json.loads(manifest_bytes);backend.pop('qualification',None)
    environment_sha=sha(enc(backend))
    env={'HOME':str(root/'home'),'LIMA_HOME':str(root/'lima'),'XDG_CACHE_HOME':str(root/'cache'),'SSH':'/usr/bin/ssh','PATH':'/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin','LANG':'en_US.UTF-8'}
    prefix=[backend['limaExecutable'],'shell','--workdir','/','worker','sudo','-n','/usr/bin/python3','/usr/local/lib/xcb-command/guest.py']
    cases=[]
    def preserve_and_ack(raw):
        value=json.loads(raw);identifier=value['custody']['commandId']
        assert value['joined'] is True
        retained=root/('qualification-case-'+identifier+'.json')
        if retained.exists():assert retained.read_bytes()==raw
        else:
            fd=os.open(retained,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
            with os.fdopen(fd,'wb') as output:output.write(raw);output.flush();os.fsync(output.fileno())
            fd=os.open(root,os.O_RDONLY)
            try:os.fsync(fd)
            finally:os.close(fd)
        ack=enc({'version':1,'custody':value['custody'],'resultSha256':sha(raw),'hostReceiptSha256':sha(raw)})
        response=subprocess.run(prefix+['ack',identifier],input=ack,env=env,check=True,capture_output=True,timeout=20)
        assert json.loads(response.stdout)=={'acknowledged':True}
    # Recover disposable scratch from failed earlier qualification attempts.
    # Only an exact retained candidate environment hash plus the fixed synthetic
    # workspace identifies these jobs; ordinary runtime backend IDs are hashes
    # of the complete qualified manifest and cannot enter this path.
    inventory="""import json,pathlib
rows=[]
for job in pathlib.Path('/var/lib/xcb-command/jobs').iterdir():
 if (job/'result.json').exists() and not (job/'ack.json').exists():
  result=json.loads((job/'result.json').read_bytes())
  if result.get('joined') is True:rows.append(result['custody'])
print(json.dumps(rows))"""
    prior=json.loads(subprocess.run(prefix[:-2]+['/usr/bin/python3','-c',inventory],env=env,check=True,capture_output=True,timeout=20).stdout)
    for custody in prior:
        candidate=root/('candidate-'+custody['backendSha256']+'.json')
        if custody['bootId']!=backend['bootId'] or custody['workspaceId']!=sha(b'synthetic-workspace') or not candidate.is_file():continue
        assert sha(candidate.read_bytes())==custody['backendSha256']
        raw=subprocess.run(prefix+['status',custody['commandId']],env=env,check=True,capture_output=True,timeout=20).stdout
        assert json.loads(raw)['custody']==custody
        preserve_and_ack(raw)
    def run(name,argv,timeout=10000,pre_cancel=False,files=None,git=None,record=True):
        identifier='cmd_'+uuid.uuid4().hex;request={'argv':argv,'cwd':'.','timeoutMs':timeout,'network':'none'}
        document={'version':1,'workspaceId':sha(b'synthetic-workspace'),'files':files or [],'directories':[]}
        if git is not None:document['git']=git
        seed=enc(document)
        custody={'version':1,'commandId':identifier,'runId':'run_'+uuid.uuid4().hex,'workspaceId':sha(b'synthetic-workspace'),'snapshotSha256':sha(seed),'requestSha256':sha(enc(request)),'backendSha256':environment_sha,'bootId':backend['bootId']}
        envelope=enc({'custody':custody,'request':request,'snapshotBase64':base64.b64encode(seed).decode()})
        if pre_cancel:subprocess.run(prefix+['cancel',identifier],env=env,check=True,capture_output=True,timeout=20)
        started=time.monotonic();completed=subprocess.run(prefix+['run'],input=envelope,capture_output=True,env=env,timeout=timeout/1000+220)
        assert completed.returncode==0,(name,completed.stderr[-1000:])
        value=json.loads(completed.stdout);assert value['custody']==custody and value['joined'],name
        assert value['cgroup']['path'].endswith(identifier+'.service/worker') and value['cgroup']['ino']>0
        if value['changesBase64'] is not None:
            delta=base64.b64decode(value['changesBase64']);assert sha(delta)==value['changesSha256']
            value['changes']=json.loads(delta)
        recovered=subprocess.run(prefix+['status',identifier],env=env,check=True,capture_output=True,timeout=20)
        assert recovered.stdout==completed.stdout,'durable recovery differs'
        if record:cases.append({'name':name,'resultSha256':sha(completed.stdout),'response':completed.stdout.decode()})
        if name!='file-edit-and-python':preserve_and_ack(completed.stdout)
        print(json.dumps({'case':name,'joined':True,'elapsedMs':round((time.monotonic()-started)*1000)}),flush=True)
        return value
    success=run('file-edit-and-python',['python3','-c','from pathlib import Path; print("success"); Path("proof.txt").write_text("ok")'])
    assert success['exitCode']==0 and success['stdout']=='success\n' and success['changes']['changes'][0]['path']=='proof.txt'
    denied=run('uid-filesystem-network-and-userns',['python3','-c', '''import os,socket,subprocess
assert os.geteuid()==61001
import stat
for fd in range(3,64):
 try: metadata=os.fstat(fd)
 except OSError: continue
 assert not stat.S_ISDIR(metadata.st_mode), 'inherited directory FD'
for path in ('/var/lib/xcb-command','/home/xcbhost','/sys/fs/cgroup','/Users','/run/dbus'):
 try: os.listdir(path)
 except OSError: pass
 else: raise AssertionError(path)
try: os.setuid(0)
except OSError as e: assert e.errno in (1,22)
else: raise AssertionError('setuid')
s=socket.socket();s.settimeout(0.5)
try:s.connect(('1.1.1.1',443))
except OSError:pass
else:raise AssertionError('network')
assert subprocess.run(['unshare','--user','true'],capture_output=True).returncode!=0
print('all-denied')'''])
    assert denied['exitCode']==0 and denied['stdout']=='all-denied\n',denied['stderr']
    detached=run('detached-setsid-closed-stdio',['python3','-c', '''import os,time
if os.fork()==0:
 os.setsid()
 if os.fork()!=0: os._exit(0)
 for fd in (0,1,2):
  try:os.close(fd)
  except OSError:pass
 time.sleep(60)
 os._exit(0)
print('leader-done')'''])
    assert detached['exitCode']==0 and not detached['timedOut']
    timeout=run('deadline-kills-descendants',['python3','-c','import os,time; os.fork(); time.sleep(60)'],200)
    assert timeout['timedOut'] and timeout['exitCode']!=0
    flood=run('output-overflow-joins',['python3','-c','import os; os.write(1,b"x"*1048576); import time; time.sleep(60)'])
    assert flood['truncated'] and len(flood['stdout'])<=256*1024
    cancelled=run('pre-cancel-never-executes',['python3','-c','from pathlib import Path;Path("must-not-exist").write_text("bad")'],pre_cancel=True)
    assert cancelled['cancelled'] and cancelled['exitCode'] is None and not cancelled['changes']['changes']
    tools=run('offline-language-toolchains',['sh','-c', '''set -eu
node -e 'console.log(42)'
bun -e 'console.log(43)'
cat >/tmp/main.rs <<'RUST'
fn main(){println!("44");}
RUST
rustc /tmp/main.rs -o /tmp/proof
/tmp/proof'''])
    assert tools['exitCode']==0 and tools['stdout']=='42\n43\n44\n',tools['stderr']
    peer_id=success['custody']['commandId'];nonce=uuid.uuid4().hex
    peer='/var/lib/xcb-command/jobs/'+peer_id
    manage='''import os,pathlib,sys
p=pathlib.Path(sys.argv[1])/'work'/'qualification-canary'
if sys.argv[3]=='create':
 fd=os.open(p,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
 with os.fdopen(fd,'w') as f:f.write(sys.argv[2])
else:
 assert p.read_text()==sys.argv[2]
 p.unlink()
'''
    subprocess.run(prefix[:-2]+['/usr/bin/python3','-c',manage,peer,nonce,'create'],env=env,check=True,capture_output=True,timeout=20)
    peer_check=run('peer-work-and-control-denied',['python3','-c', '''import os,sys
peer=sys.argv[1]
for path in (peer+'/work/qualification-canary',peer+'/custody.json',peer+'/seed.json','/var/lib/xcb-command/admission.lock'):
 try: fd=os.open(path,os.O_RDONLY)
 except OSError: pass
 else: os.close(fd);raise AssertionError('peer pathname escape')
for fd in range(64):
 for path in ('../../'''+peer_id+'''/work/qualification-canary','../../../admission.lock'):
  try: result=os.open(path,os.O_RDONLY,dir_fd=fd)
  except OSError: pass
  else: os.close(result);raise AssertionError('peer openat escape')
print('peer-denied')''',peer])
    assert peer_check['exitCode']==0 and peer_check['stdout']=='peer-denied\n',peer_check['stderr']
    subprocess.run(prefix[:-2]+['/usr/bin/python3','-c',manage,peer,nonce,'verify'],env=env,check=True,capture_output=True,timeout=20)
    projection_path=pathlib.Path(__file__).with_name('git_projection.py')
    projection_test_path=pathlib.Path(__file__).with_name('test_git_projection.py')
    assert sha(projection_path.read_bytes())==backend['gitProjectionSha256']
    assert sha(projection_test_path.read_bytes())==backend['gitProjectionTestsSha256']
    def file_entry(path,data):
        return {'path':path,'base64':base64.b64encode(data).decode(),'sha256':sha(data),'executable':False}
    test_files=[file_entry('git_projection.py',projection_path.read_bytes()),file_entry('test_git_projection.py',projection_test_path.read_bytes())]
    tests=run('git-projection-unit-semantics',['python3','-B','-c', "import unittest,test_git_projection as t; result=unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromModule(t)); assert result.testsRun==11 and not result.skipped and result.wasSuccessful(); print('git-tests-passed')"],60000,files=test_files)
    assert tests['exitCode']==0 and tests['stdout']=='git-tests-passed\n',tests['stderr']
    # Fixture creation uses pure bytes/zlib only; no host Git or credentials.
    sys.dont_write_bytecode=True
    spec=importlib.util.spec_from_file_location('projection_fixture',projection_test_path)
    fixture=importlib.util.module_from_spec(spec);spec.loader.exec_module(fixture)
    projected=run('readonly-filtered-git-inspection',['python3','-c', r"""import pathlib,subprocess
assert subprocess.check_output(['git','status','--porcelain'])==b'MM input.txt\n'
assert b'-staged' in subprocess.check_output(['git','diff','--','input.txt'])
assert b'+working' in subprocess.check_output(['git','diff','--','input.txt'])
assert b'-base' in subprocess.check_output(['git','diff','--cached','--','input.txt'])
assert b'+staged' in subprocess.check_output(['git','diff','--cached','--','input.txt'])
assert subprocess.check_output(['git','rev-list','--count','HEAD']).strip()==b'1'
assert subprocess.check_output(['git','ls-files'])==b'input.txt\n'
assert b'PRIVATE_' not in subprocess.check_output(['git','log','--format=fuller'])
for path in ('.git/config','.git/hooks/test','.git/index','.git/HEAD'):
 try: pathlib.Path(path).write_text('forbidden')
 except OSError: pass
 else: raise AssertionError('Git metadata writable')
assert not pathlib.Path('.git/objects/info/alternates').exists()
print('git-projected-readonly')"""],files=[file_entry('input.txt',b'working\n')],git=fixture.fixture())
    assert projected['exitCode']==0 and projected['stdout']=='git-projected-readonly\n',projected['stderr']
    assert not projected['changes']['changes'],'Git projection leaked into host changes'
    # Actual public archives, pinned by retained lockfile integrity values.
    # Only the dedicated fixed preloader has network; both proof commands are
    # ordinary offline workers using exact manifest-derived readonly caches.
    fixture={'version': 1, 'files': {'Cargo.toml': '[package]\nname = "xcb-cache-fixture"\nversion = "0.0.0"\nedition = "2024"\n[dependencies]\ncfg-if = "=1.0.5"\n', 'Cargo.lock': 'version = 4\n\n[[package]]\nname = "cfg-if"\nversion = "1.0.5"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "4e7648175b45a9a48536d676f68d918270699102aa8dab5496df06904c914600"\n\n[[package]]\nname = "xcb-cache-fixture"\nversion = "0.0.0"\ndependencies = ["cfg-if"]\n', 'package.json': '{"dependencies": {"bytes": "3.1.2"}, "name": "xcb-cache-fixture", "private": true, "scripts": {"postinstall": "node -e \\"require(\'fs\').writeFileSync(\'/tmp/xcb-postinstall-sentinel\',\'ran\')\\""}, "version": "0.0.0"}\n', 'bun.lock': '{"configVersion": 1, "lockfileVersion": 1, "packages": {"bytes": ["bytes@3.1.2", "", {}, "sha512-/Nf7TyzTx6S3yRJObOAV7956r8cr2+Oj8AC5dt8wSP3BQAoeX58NoHyCU8P8zGkNXStjTSi6fzO6F0pBdcYbEg=="]}, "workspaces": {"": {"dependencies": {"bytes": "3.1.2"}, "name": "xcb-cache-fixture"}}}\n', 'src/main.rs': 'fn main() { cfg_if::cfg_if! { if #[cfg(target_os = "linux")] { println!("42"); } else { panic!("guest platform"); } } }\n'}, 'commands': [{'argv': ['cargo', 'run', '--offline', '--locked', '--quiet'], 'stdout': '42\n'}, {'argv': ['bun', 'install', '--frozen-lockfile', '--ignore-scripts'], 'stdout': None}, {'argv': ['bun', '-e', 'if(require("bytes")("1kb")!==1024) process.exit(1); if(require("fs").existsSync("/tmp/xcb-postinstall-sentinel")) process.exit(2); console.log("1024")'], 'stdout': '1024\n'}], 'publicSources': {'crateSourceLockSha256': 'e587937772986efb6280de34650bb53750b0c3f4157e2ce1ca79704fba64ca87', 'npmSourceLockSha256': '1548acdd3800410855d2c9111c5b4b93211f9e5fda5321ee61129d45073b9a04'}, 'preloaderPostinstallSentinel': '/tmp/xcb-postinstall-sentinel'}
    fixture_files=[file_entry(path,text.encode()) for path,text in sorted(fixture['files'].items())]
    spec={'version':1,'toolDigests':{name:backend['toolDigests'][name]['sha256'] for name in ('bun','cargo','git','node','python','rustc')},
          'files':[{k:row[k] for k in ('path','base64','sha256')} for row in fixture_files if row['path']!='src/main.rs']}
    def cache_call(operation,value,timeout=60):
        response=subprocess.run(prefix+[operation],input=enc(value),env=env,capture_output=True,timeout=timeout)
        assert response.returncode==0,(operation,response.stderr[-1000:])
        return response.stdout,json.loads(response.stdout)
    def cache_ack(key,raw):
        retained=root/('qualification-cache-'+sha(raw)+'.json')
        if retained.exists():assert retained.read_bytes()==raw
        else:
            fd=os.open(retained,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
            with os.fdopen(fd,'wb') as out:out.write(raw);out.flush();os.fsync(out.fileno())
            fd=os.open(root,os.O_RDONLY)
            try:os.fsync(fd)
            finally:os.close(fd)
        _,ack=cache_call('public-cache-ack',{'version':1,'cacheKey':key,'hostReceiptSha256':sha(raw)})
        assert ack=={'acknowledged':True}
    raw,plan=cache_call('public-cache-plan',spec)
    key=plan['cacheKey']
    assert sha(enc({k:v for k,v in plan.items() if k!='cacheKey'}))==key
    assert plan['inputs']==[{'path':row['path'],'sha256':row['sha256']} for row in spec['files']]
    assert plan['toolDigests']==spec['toolDigests'] and plan['preloaderSha256']==backend['publicCacheSha256']
    assert len(plan['archives'])==2 and not plan['repositories']
    cache_ack(key,raw)
    raw,prepared=cache_call('public-cache-prepare',{'version':1,'expectedCacheKey':key,'spec':spec},1300)
    assert set(prepared)=={'version','cacheKey','joined','inventorySha256','bytes','entries'} and prepared['joined'] is True and prepared['cacheKey']==key
    assert prepared['version']==1 and 0<prepared['bytes']<=1024**3 and 0<prepared['entries']<=200000
    _,status=cache_call('public-cache-status',{'version':1,'cacheKey':key})
    assert status=={'version':1,'cacheKey':key,'joined':True,'prepared':True,'receipt':prepared}
    cache_ack(key,raw)
    # Check the exact retained materializer scratch, not another process's /tmp.
    no_script="""import json,pathlib,sys
root=pathlib.Path('/var/lib/xcb-command')
record=json.loads((root/'cache'/sys.argv[1]/'record.json').read_bytes())
job=root/'preloads'/record['attemptId']
assert not (job/'work/materialize-scratch/xcb-postinstall-sentinel').exists()
for name in ('plan','fetch','materialize'):
 phase=json.loads((job/(name+'-phase.json')).read_bytes())
 assert phase['joined'] is True and phase['exitCode']==0 and not any(phase[k] for k in ('error','truncated','cancelled','timedOut'))
print('ignored-scripts-and-joined')"""
    checked=subprocess.run(prefix[:-2]+['/usr/bin/python3','-c',no_script,key],env=env,check=True,capture_output=True,timeout=30)
    assert checked.stdout==b'ignored-scripts-and-joined\n'
    bounded=run('public-cache-isolation-and-key-binding',['python3','-c',r"""import pathlib,os,socket
assert pathlib.Path('/opt/xcb-cache/cargo/vendor').is_dir()
assert pathlib.Path('/opt/xcb-cache/bun').is_dir()
for path in ('/opt/xcb-cache/probe','/opt/xcb-cache/cargo/config.toml','/home/xcb/.cargo/config.toml'):
 try: pathlib.Path(path).write_text('forbidden')
 except OSError: pass
 else: raise AssertionError('cache writable')
s=socket.socket();s.settimeout(.5)
try:s.connect(('1.1.1.1',443))
except OSError:pass
else:raise AssertionError('worker network allowed')
print('cache-isolation-key-binding')"""],files=fixture_files)
    assert bounded['exitCode']==0 and bounded['stdout']=='cache-isolation-key-binding\n',bounded['stderr']
    changed=[dict(row) for row in fixture_files]
    for index,row in enumerate(changed):
        if row['path']=='Cargo.toml':changed[index]=file_entry('Cargo.toml',(fixture['files']['Cargo.toml']+'\n').encode())
    mismatch=run('cache-key-mismatch-negative',['python3','-c',"from pathlib import Path; assert not Path('/opt/xcb-cache').exists(); print('cache-key-miss')"],files=changed,record=False)
    assert mismatch['exitCode']==0 and mismatch['stdout']=='cache-key-miss\n' and 'no prepared dependency cache matches' in mismatch['stderr']
    built=run('offline-cargo-bun-cache-usage',['python3','-c',r"""import subprocess,pathlib
cargo=subprocess.run(['cargo','run','--offline','--locked','--quiet'],capture_output=True,check=True)
assert cargo.stdout==b'42\n'
subprocess.run(['bun','install','--frozen-lockfile','--ignore-scripts'],capture_output=True,check=True)
node=subprocess.run(['bun','-e','console.log(require("bytes")("1kb"))'],capture_output=True,check=True)
assert node.stdout==b'1024\n'
assert not pathlib.Path('/tmp/xcb-postinstall-sentinel').exists()
print('offline-cargo-bun-passed')"""],timeout=120000,files=fixture_files)
    assert built['exitCode']==0 and built['stdout']=='offline-cargo-bun-passed\n',built['stderr']
    assert not built['changes']['changes'],'generated dependency/build files leaked into host publication'
    # Recheck the exact runtime after all cases, not just before a potentially
    # long setup. No partial or failed suite ever publishes a receipt.
    observation=json.loads(subprocess.run(prefix+['inspect'],env=env,check=True,capture_output=True,timeout=20).stdout)
    assert observation=={'version':1,'bootId':backend['bootId'],'agentSha256':backend['guestSha256'],'bwrapSha256':backend['bwrapSha256'],'toolDigests':backend['toolDigests'],'gitProjectionSha256':backend['gitProjectionSha256'],'publicCacheSha256':backend['publicCacheSha256']}
    assert sha(pathlib.Path(backend['limaExecutable']).read_bytes())==backend['limaSha256']
    assert (args.candidate_manifest or root/'backend.json').read_bytes()==manifest_bytes
    result={'version':1,'environmentSha256':environment_sha,'suiteSha256':sha(pathlib.Path(__file__).read_bytes()),'cases':cases}
    args.output.parent.mkdir(mode=0o700,parents=True,exist_ok=True)
    fd=os.open(args.output,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
    with os.fdopen(fd,'wb') as output:output.write(enc(result));output.flush();os.fsync(output.fileno())
    directory=os.open(args.output.parent,os.O_RDONLY)
    try:os.fsync(directory)
    finally:os.close(directory)
    for case in cases:
        value=json.loads(case['response']);identifier=value['custody']['commandId']
        ack=enc({'version':1,'custody':value['custody'],'resultSha256':case['resultSha256'],'hostReceiptSha256':sha(enc(result))})
        acknowledged=subprocess.run(prefix+['ack',identifier],input=ack,env=env,check=True,capture_output=True,timeout=20)
        assert json.loads(acknowledged.stdout)=={'acknowledged':True}
        recovered=subprocess.run(prefix+['status',identifier],env=env,check=True,capture_output=True,timeout=20)
        assert recovered.stdout.decode()==case['response'],'cleanup changed retained recovery evidence'
    print(json.dumps({'passed':True,'checks':len(cases),'output':str(args.output)}))

if __name__=='__main__':main()
