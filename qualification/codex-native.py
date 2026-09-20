#!/usr/bin/python3
"""Exact production Seatbelt: synthetic host-RPC canaries, no login or model turns."""
import argparse, base64, hashlib, json, os, pathlib, re, selectors, signal, stat, subprocess, sys, time, uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--executable', required=True, type=pathlib.Path)
parser.add_argument('--output', required=True, type=pathlib.Path)
parser.add_argument('--preplanted-aliases', action='store_true', help='negative control: deliberately violate host single-link admission')
args = parser.parse_args()
assert sys.platform == 'darwin', 'This fixture requires macOS Seatbelt'
REPO = pathlib.Path(__file__).resolve().parents[1]
ROOT = args.output.resolve()
ROOT.mkdir(mode=0o700, parents=True, exist_ok=True)
BIN = args.executable.resolve(strict=True)
config_source = (REPO/'crates/xcb-runtime/src/codex/config.rs').read_text()
EXPECTED = re.search(r'pub const BINARY_SHA256: &str = "([a-f0-9]{64})"', config_source).group(1)
adversarial_aliases = args.preplanted_aliases
sha = lambda b: hashlib.sha256(b).hexdigest()

def public_ca_copy(path):
    # Only the OS-owned public trust bundle is read; never user credentials or
    # Keychain state. Bind a stable regular source and an isolated single name.
    with os.fdopen(os.open('/private/etc/ssl/cert.pem',os.O_RDONLY|os.O_NOFOLLOW|os.O_CLOEXEC),'rb') as source:
        before=os.fstat(source.fileno())
        assert stat.S_ISREG(before.st_mode) and before.st_uid==0 and before.st_nlink==1
        assert before.st_mode & 0o022==0 and 0<before.st_size<=2*1024*1024
        contents=source.read(2*1024*1024+1)
        after=os.fstat(source.fileno())
        identity=lambda value:(value.st_dev,value.st_ino,value.st_mode,value.st_uid,value.st_gid,value.st_nlink,value.st_size,value.st_mtime_ns,value.st_ctime_ns)
        assert len(contents)==before.st_size and identity(before)==identity(after)
        assert identity(after)==identity(os.lstat('/private/etc/ssl/cert.pem'))
    assert b'-----BEGIN CERTIFICATE-----' in contents
    with path.open('xb') as target:target.write(contents)
    path.chmod(0o600)
    assert path.stat().st_nlink==1
    return contents

def file_identity(path):
    value=path.lstat()
    return [value.st_dev,value.st_ino,value.st_mode,value.st_uid,value.st_gid,value.st_nlink,value.st_mtime_ns,value.st_ctime_ns,value.st_size,value.st_flags]

def stable_setup(paths):
    # macOS can asynchronously attach provenance/document tracking after file
    # creation. Finish that trusted setup before recording the launch baseline;
    # keep ctime, flags, identity, permissions, size and bytes strict thereafter.
    before={str(path):file_identity(path) for path in paths}
    previous=before
    deadline=time.monotonic()+5
    quiet_since=time.monotonic()
    while time.monotonic()<deadline:
        time.sleep(0.05)
        current={str(path):file_identity(path) for path in paths}
        if current!=previous:
            previous=current;quiet_since=time.monotonic()
        elif time.monotonic()-quiet_since>=0.25:
            return {'before':before,'after':current}
    raise RuntimeError('trusted fixture setup metadata did not stabilize')

with BIN.open('rb') as f:
    binary = f.read(512 * 1024 * 1024 + 1)
assert len(binary) <= 512 * 1024 * 1024 and sha(binary) == EXPECTED, 'Provider is not the admitted executable'
marker = b'{\n  "models":'
assert binary.count(marker) == 1, 'Ambiguous bundled catalog'
offset = binary.index(marker)
# The exact executable embeds its own catalog; no owner model cache is read.
bundled = binary[offset:offset + 4 * 1024 * 1024].decode('utf-8', 'replace')
data, _ = json.JSONDecoder().raw_decode(bundled)
del binary, bundled
run = ROOT/('run-'+uuid.uuid4().hex)
run.mkdir(mode=0o700)
scratch = run/'scratch'; profile = scratch/'profile'; home = scratch/'home'; cwd = scratch/'work'
for path in [scratch, profile, home, home/'tmp', cwd, run/'consumer', run/'peer-account']:
    path.mkdir(mode=0o700)
exe = run/'provider'
subprocess.run(['/bin/cp','-c',str(BIN),str(exe)],check=True)
exe.chmod(0o500)
ca_bundle=run/'public-ca.pem'
ca_bytes=public_ca_copy(ca_bundle)
catalog = run/'models.json'
rows=[]
for row in data['models']:
    if row['slug'] not in ['gpt-6-astra','gpt-5.6-sol']: continue
    row.update(tool_mode='direct',shell_type='disabled',apply_patch_tool_type=None,
               experimental_supported_tools=[],supports_search_tool=False,
               supports_experimental_context=False,multi_agent_version='disabled',node_repl_disabled=True)
    rows.append(row)
catalog.write_text(json.dumps({'models':rows},separators=(',',':'),ensure_ascii=False,sort_keys=True));catalog.chmod(0o600)
config = profile/'config.toml'
# Reconstruct Rust's fixed string arrays to bind the config to current source.
config_source=(REPO/'crates/xcb-runtime/src/codex/config.rs').read_text()
features_block=config_source.split('pub const ACCOUNT_FEATURES: &[&str] = &[',1)[1].split('];',1)[0]
features=[json.loads(x) for x in re.findall(r'"(?:[^"\\]|\\.)*"',features_block)]
configuration_source=config_source.split('pub fn configuration(',1)[1].split('\npub fn thread_configuration(',1)[0]
array_blocks=re.findall(r'lines\.extend\(\s*\[(.*?)\]\s*\.map\(str::to_owned\)',configuration_source,re.S)
assert len(array_blocks)==2
arrays=[[json.loads(x) for x in re.findall(r'"(?:[^"\\]|\\.)*"',block)] for block in array_blocks]
baseline='\n'.join(arrays[0]+[name+' = false' for name in features]+arrays[1])
config.write_text('model_catalog_json = '+json.dumps(str(catalog))+'\n'+baseline);config.chmod(0o600)
source=(REPO/'crates/xcb-runtime/src/sandbox.rs').read_text()
function=source.split('pub fn codex_seatbelt(',1)[1].split('\n/// Devin',1)[0]
template=re.search(r'r#"(.*?)"#',function,re.S).group(1)
policy=template
for name,path in {'exe':exe,'work':scratch,'profile':profile,'config':config,'catalog':catalog,'ca_bundle':ca_bundle}.items():
    assert path.resolve()==path
    policy=policy.replace('{'+name+'}',json.dumps(str(path)))
assert not re.search(r'\{(?:exe|work|profile|config|catalog|ca_bundle)\}',policy)
assert 'com.apple.trustd' not in policy and 'Keychains' not in policy, 'ambient trust access must stay denied'
(run/'production.sb').write_text(policy)
(run/'sandbox-function.rs.txt').write_text('pub fn codex_seatbelt('+function)
outside=run/'consumer/canary'; peer=run/'peer-account/canary'; ambient=run/'ambient-config'
for path in [outside,peer,ambient]: path.write_text('XCB SYNTHETIC PRIVATE CANARY\n');path.chmod(0o600)
inside=cwd/'canary';inside.write_text('XCB SYNTHETIC SCRATCH CANARY\n')
target=cwd/'replacement';target.write_text('XCB SYNTHETIC CONFIG REPLACEMENT\n')
(cwd/'outside-link').symlink_to(outside)
(cwd/'config-link').symlink_to(config)
(cwd/'catalog-link').symlink_to(catalog)
(cwd/'ca-link').symlink_to(ca_bundle)
# Hard-link aliases exercise vnode/path policy rather than assuming path spelling.
if adversarial_aliases:
    os.link(config,cwd/'config-hardlink');os.link(catalog,cwd/'catalog-hardlink');os.link(ca_bundle,cwd/'ca-hardlink')
else:
    # Mirrors the host's private::read single-name precondition before admission.
    assert all(path.stat().st_nlink==1 for path in [config,catalog,ca_bundle])
protected_paths=[outside,peer,ambient,config,catalog,ca_bundle]
setup_metadata=stable_setup(protected_paths)
initial={str(path):sha(path.read_bytes()) for path in protected_paths}
initial_metadata={str(path):file_identity(path) for path in protected_paths}
env={'HOME':str(home),'CODEX_HOME':str(profile),'PATH':'/usr/bin:/bin:/usr/sbin:/sbin',
     'LANG':'en_US.UTF-8','NO_COLOR':'1','XDG_CONFIG_HOME':str(home/'.config'),
     'XDG_DATA_HOME':str(home/'.local/share'),'XDG_CACHE_HOME':str(home/'.cache'),
     'TMPDIR':str(home/'tmp'),'CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED':'1',
     'SSL_CERT_FILE':str(ca_bundle)}
p=subprocess.Popen(['/usr/bin/sandbox-exec','-f',str(run/'production.sb'),str(exe),
                    'app-server','--strict-config','--listen','stdio://'],
    cwd=cwd,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True)
sel=selectors.DefaultSelector()
for stream in [p.stdout,p.stderr]: os.set_blocking(stream.fileno(),False);sel.register(stream,selectors.EVENT_READ)
buf=b'';stderr=bytearray();frames=[];ids=0;total=0
receipt={'runDirectory':str(run),'binarySha256':EXPECTED,'sandboxFunctionSha256':sha(function.encode()),
         'probeSha256':sha(pathlib.Path(__file__).read_bytes()),
         'sandboxSha256':sha(policy.encode()),'catalogSha256':initial[str(catalog)],
         'configSha256':initial[str(config)],'publicCaSha256':initial[str(ca_bundle)],'caEnvironment':'SSL_CERT_FILE','auth':'fresh-empty-profile','modelTurns':0,
         'preplantedHardlinks':adversarial_aliases,'launchSingleLinkGuardPassed':not adversarial_aliases,
         'checks':[],'notifications':[],'errors':[], 'setupMetadata':setup_metadata}
def send(value):
    p.stdin.write((json.dumps(value,separators=(',',':'))+'\n').encode());p.stdin.flush()
def pump(deadline):
    global buf,total
    for key,_ in sel.select(max(0,min(.1,deadline-time.monotonic()))):
        chunk=os.read(key.fileobj.fileno(),65536)
        if not chunk: sel.unregister(key.fileobj);continue
        total+=len(chunk)
        if total>16*1024*1024: raise RuntimeError('output bound')
        if key.fileobj is p.stderr:
            stderr.extend(chunk)
            if len(stderr)>256*1024: raise RuntimeError('stderr bound')
        else:
            buf+=chunk
            while b'\n' in buf:
                line,buf=buf.split(b'\n',1);f=json.loads(line);frames.append(f)
                if 'method' in f:
                    receipt['notifications'].append(f['method'])
                    if 'id' in f: raise RuntimeError('unexpected provider request')
def rpc(method,params):
    global ids
    ids+=1;current=ids;send({'id':current,'method':method,'params':params});deadline=time.monotonic()+15
    while time.monotonic()<deadline:
        for f in frames:
            if f.get('id')==current and 'method' not in f:return f
        pump(deadline)
        if p.poll() is not None:raise RuntimeError('native exited before '+method)
    raise RuntimeError('rpc timeout '+method)
def check(label,method,params,allowed,expected_data=None):
    before={str(path):file_identity(path) for path in protected_paths}
    f=rpc(method,params);ok='result' in f
    record={'label':label,'method':method,'expectedAllowed':allowed,'allowed':ok,'matched':ok==allowed}
    record['metadataChanges']={str(path):{'before':before[str(path)],'after':file_identity(path)} for path in protected_paths if file_identity(path)!=before[str(path)]}
    if expected_data is not None and ok:
        record['matched'] &= base64.b64decode(f['result']['dataBase64'])==expected_data
    if 'error' in f:record['error']=f['error']
    receipt['checks'].append(record)
    return f
def write(path):return {'path':str(path),'dataBase64':base64.b64encode(b'SYNTHETIC WRITE\n').decode()}
try:
    f=rpc('initialize',{'clientInfo':{'name':'xcb-production-canary','version':'0.4.0'},'capabilities':{'experimentalApi':True}})
    receipt['metadataAfterInitialize']={str(path):file_identity(path) for path in protected_paths}
    assert f['result']['codexHome']==str(profile)
    send({'method':'initialized'})
    f=rpc('account/read',{'refreshToken':False});assert f['result']['account'] is None
    f=rpc('config/read',{'cwd':str(cwd),'includeLayers':True});assert f['result']['config']['model_catalog_json']==str(catalog)
    f=rpc('model/list',{'includeHidden':True,'limit':100});receipt['models']=[m['id'] for m in f['result']['data']]
    receipt['metadataAfterStartup']={str(path):file_identity(path) for path in protected_paths}
    check('scratch read','fs/readFile',{'path':str(inside)},True,inside.read_bytes())
    check('scratch write','fs/writeFile',write(cwd/'written'),True)
    check('profile refresh write','fs/writeFile',write(profile/'synthetic-refresh'),True)
    for label,path in [('consumer',outside),('peer account',peer),('ambient config',ambient),('consumer symlink',cwd/'outside-link')]:
        check(label+' read denied','fs/readFile',{'path':str(path)},False)
        check(label+' write denied','fs/writeFile',write(path),False)
    protected=[('config',config),('catalog',catalog),('config symlink',cwd/'config-link'),('catalog symlink',cwd/'catalog-link'),('public CA',ca_bundle),('public CA symlink',cwd/'ca-link')]
    if adversarial_aliases:protected += [('config hardlink',cwd/'config-hardlink'),('catalog hardlink',cwd/'catalog-hardlink'),('public CA hardlink',cwd/'ca-hardlink')]
    for label,path in protected:
        check(label+' read','fs/readFile',{'path':str(path)},True,ca_bytes if label.startswith('public CA') else None)
        check(label+' write denied','fs/writeFile',write(path),False)
    for label,path in [('config',config),('catalog',catalog),('public CA',ca_bundle)]:
        check(label+' copy-over denied','fs/copy',{'sourcePath':str(target),'destinationPath':str(path)},False)
        check(label+' unlink denied','fs/remove',{'path':str(path),'recursive':False,'force':False},False)
    check('helper execution denied','process/spawn',{'command':['/usr/bin/true'],'cwd':str(cwd),'processHandle':'canary','timeoutMs':1000,'outputBytesCap':1024},False)
    check('self execution denied','process/spawn',{'command':[str(exe),'--version'],'cwd':str(cwd),'processHandle':'self-canary','timeoutMs':1000,'outputBytesCap':1024},False)
    # Last: tree deletion must fail on the protected config/profile boundary.
    check('profile recursive removal denied','fs/remove',{'path':str(profile),'recursive':True,'force':False},False)
except Exception as error:receipt['errors'].append(str(error))
finally:
    try:p.stdin.close()
    except BrokenPipeError:pass
    deadline=time.monotonic()+3
    try:
        while p.poll() is None and time.monotonic()<deadline:pump(deadline)
    except Exception as error:receipt['errors'].append('cleanup drain: '+str(error))
    if p.poll() is None:os.killpg(p.pid,signal.SIGKILL)
    try:p.wait(timeout=5)
    except subprocess.TimeoutExpired:receipt['errors'].append('root join timeout')
    deadline=time.monotonic()+3
    try:
        while sel.get_map() and time.monotonic()<deadline:pump(deadline)
    except Exception as error:receipt['errors'].append('final drain: '+str(error))
    try:os.killpg(p.pid,0);absent=False
    except ProcessLookupError:absent=True
    receipt.update(rootExitCode=p.returncode,stdioJoined=not bool(sel.get_map()) and not buf,processGroupAbsent=absent,
                   binaryUnchanged=sha(exe.read_bytes())==EXPECTED,stderr=stderr.decode('utf8','replace'))
    receipt['protectedFilesUnchanged']={str(path):path.exists() and sha(path.read_bytes())==digest for path,digest in [(pathlib.Path(k),v) for k,v in initial.items()]}
    receipt['protectedMetadata']={str(path):{'before':initial_metadata[str(path)],'after':file_identity(path) if path.exists() else None} for path in protected_paths}
    receipt['protectedMetadataUnchanged']={str(path):path.exists() and file_identity(path)==initial_metadata[str(path)] for path in protected_paths}
    receipt['sourceFunctionUnchanged']=(REPO/'crates/xcb-runtime/src/sandbox.rs').read_text().split('pub fn codex_seatbelt(',1)[1].split('\n/// Devin',1)[0]==function
    (run/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
    (ROOT/'latest-receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
    failures=[c for c in receipt['checks'] if not c['matched']]
    print(json.dumps({'receipt':str(run/'receipt.json'),'checks':len(receipt['checks']),'failures':failures,'errors':receipt['errors'],
        'rootExitCode':p.returncode,'stdioJoined':receipt['stdioJoined'],'processGroupAbsent':absent,
        'protectedFilesUnchanged':all(receipt['protectedFilesUnchanged'].values()),'protectedMetadataUnchanged':all(receipt['protectedMetadataUnchanged'].values()),'sourceFunctionUnchanged':receipt['sourceFunctionUnchanged']}))
    raise SystemExit(bool(failures or receipt['errors']) or p.returncode != 0 or len(receipt['checks']) != (38 if adversarial_aliases else 32) or not all(receipt['protectedFilesUnchanged'].values()) or not all(receipt['protectedMetadataUnchanged'].values()) or not absent or not receipt['stdioJoined'] or not receipt['binaryUnchanged'] or not receipt['sourceFunctionUnchanged'])
