#!/usr/bin/python3
"""Separate syscall probe: apply the exact policy after trusted test setup."""
import argparse, ctypes, hashlib, json, os, pathlib, re, signal, stat, subprocess, sys, time, uuid

REPO=pathlib.Path(__file__).resolve().parents[1]
sha=lambda b:hashlib.sha256(b).hexdigest()

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

if len(sys.argv)>1 and sys.argv[1]=='--child':
    run=pathlib.Path(sys.argv[2]);scratch=run/'scratch';profile=scratch/'profile';cwd=scratch/'work'
    config=profile/'config.toml';catalog=run/'catalog.json';ca_bundle=run/'public-ca.pem'
    ca_bytes=ca_bundle.read_bytes()
    policy=(run/'production.sb').read_bytes()
    library=ctypes.CDLL('/usr/lib/libsandbox.1.dylib',use_errno=True)
    library.sandbox_init.argtypes=[ctypes.c_char_p,ctypes.c_uint64,ctypes.POINTER(ctypes.c_char_p)]
    library.sandbox_init.restype=ctypes.c_int
    error=ctypes.c_char_p()
    status=library.sandbox_init(policy,0,ctypes.byref(error))
    if status:
        print(json.dumps({'initializationError':error.value.decode() if error.value else str(status)}));sys.exit(1)
    checks=[]
    def check(label,allowed,operation):
        record={'label':label,'expectedAllowed':allowed}
        record['publicCaBefore']=file_identity(ca_bundle)
        try:operation();record['allowed']=True
        except OSError as e:record.update(allowed=False,errno=e.errno)
        record['publicCaAfter']=file_identity(ca_bundle) if ca_bundle.exists() else None
        record['matched']=allowed is None or record['allowed']==allowed;checks.append(record)
    # Observational control: scratch hard links are not a production capability
    # requirement. Denial here means the policy forbids all alias creation.
    check('scratch hard-link creation (observational)',None,lambda:os.link(cwd/'canary',cwd/'scratch-hardlink'))
    check('config hard-link creation denied',False,lambda:os.link(config,cwd/'config-hardlink'))
    check('catalog hard-link creation denied',False,lambda:os.link(catalog,cwd/'catalog-hardlink'))
    check('config symlink creation (observational)',None,lambda:os.symlink(config,cwd/'config-symlink'))
    if (cwd/'config-symlink').is_symlink():
        check('config symlink write denied',False,lambda:(cwd/'config-symlink').write_bytes(b'changed'))
    check('config chmod denied',False,lambda:os.chmod(config,0o777))
    check('config rename denied',False,lambda:os.rename(config,cwd/'moved-config'))
    check('config overwrite rename denied',False,lambda:os.rename(cwd/'replacement',config))
    check('profile rename denied',False,lambda:os.rename(profile,scratch/'moved-profile'))
    def read_ca(path):
        assert path.read_bytes()==ca_bytes, 'public CA content changed'
    check('public CA read',True,lambda:read_ca(ca_bundle))
    check('public CA symlink read',True,lambda:read_ca(cwd/'ca-symlink'))
    check('public CA write denied',False,lambda:ca_bundle.write_bytes(b'changed'))
    check('public CA symlink write denied',False,lambda:(cwd/'ca-symlink').write_bytes(b'changed'))
    check('public CA hard-link creation denied',False,lambda:os.link(ca_bundle,cwd/'ca-hardlink'))
    check('public CA chmod denied',False,lambda:os.chmod(ca_bundle,0o777))
    check('public CA symlink chmod denied',False,lambda:os.chmod(cwd/'ca-symlink',0o777))
    check('public CA rename denied',False,lambda:os.rename(ca_bundle,cwd/'moved-ca'))
    check('public CA overwrite rename denied',False,lambda:os.rename(cwd/'ca-replacement',ca_bundle))
    check('public CA unlink denied',False,lambda:ca_bundle.unlink())
    # If source aliasing was permitted, prove its actual consequence in this
    # disposable test instead of treating successful link() as only theoretical.
    for label,path in [('config',cwd/'config-hardlink'),('catalog',cwd/'catalog-hardlink'),('public CA',cwd/'ca-hardlink')]:
        if path.exists():check(label+' post-launch hard-link write denied',False,lambda path=path:path.write_bytes(b'changed'))
    print(json.dumps({'checks':checks},separators=(',',':')))
    sys.exit(0)

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--output', required=True, type=pathlib.Path)
args=parser.parse_args()
assert sys.platform == 'darwin', 'This fixture requires macOS Seatbelt'
ROOT=args.output.resolve();ROOT.mkdir(mode=0o700,parents=True,exist_ok=True)
run=ROOT/('kernel-'+uuid.uuid4().hex);run.mkdir(mode=0o700)
scratch=run/'scratch';profile=scratch/'profile';cwd=scratch/'work'
for p in [scratch,profile,cwd]:p.mkdir(mode=0o700)
config=profile/'config.toml';catalog=run/'catalog.json';exe=run/'provider'
for p in [config,catalog,cwd/'canary',cwd/'replacement',cwd/'ca-replacement',exe]:p.write_bytes(b'SYNTHETIC CANARY\n');p.chmod(0o600)
os.link(cwd/'canary',cwd/'unsandboxed-link-control');(cwd/'unsandboxed-link-control').unlink()
ca_bundle=run/'public-ca.pem'
public_ca_copy(ca_bundle)
(cwd/'ca-symlink').symlink_to(ca_bundle)
protected_paths=[config,catalog,ca_bundle]
setup_metadata=stable_setup(protected_paths)
initial={str(p):sha(p.read_bytes()) for p in protected_paths}
initial_metadata={str(p):file_identity(p) for p in protected_paths}
source=(REPO/'crates/xcb-runtime/src/sandbox.rs').read_text()
function=source.split('pub fn codex_seatbelt(',1)[1].split('\n/// Devin',1)[0]
policy=re.search(r'r#"(.*?)"#',function,re.S).group(1)
for name,p in {'exe':exe,'work':scratch,'profile':profile,'config':config,'catalog':catalog,'ca_bundle':ca_bundle}.items():
    assert p.resolve()==p;policy=policy.replace('{'+name+'}',json.dumps(str(p)))
assert not re.search(r'\{(?:exe|work|profile|config|catalog|ca_bundle)\}',policy)
assert 'com.apple.trustd' not in policy and 'Keychains' not in policy, 'ambient trust access must stay denied'
(run/'production.sb').write_text(policy)
env={'PATH':'/usr/bin:/bin','HOME':str(scratch),'LANG':'en_US.UTF-8'}
p=subprocess.Popen(['/usr/bin/python3',str(pathlib.Path(__file__).resolve()),'--child',str(run)],
                   env=env,cwd=cwd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True)
try:out,err=p.communicate(timeout=15)
except subprocess.TimeoutExpired:
    os.killpg(p.pid,signal.SIGKILL);out,err=p.communicate(timeout=5)
assert len(out)<64*1024 and len(err)<64*1024
try:os.killpg(p.pid,0);absent=False
except ProcessLookupError:absent=True
try:result=json.loads(out)
except Exception:result={'error':'invalid helper output','stdout':out.decode('utf8','replace')}
receipt={'kind':'kernel-syscall-policy-probe-not-provider-runtime','setupMetadata':setup_metadata,'sandboxFunctionSha256':sha(function.encode()),
         'probeSha256':sha(pathlib.Path(__file__).read_bytes()),
         'sandboxSha256':sha(policy.encode()),'publicCaSha256':initial[str(ca_bundle)],'rootExitCode':p.returncode,'stdioJoined':True,'processGroupAbsent':absent,
         'stderr':err.decode('utf8','replace'),'result':result,
         'protectedFilesUnchanged':{str(path):path.exists() and sha(path.read_bytes())==digest for path,digest in [(pathlib.Path(k),v) for k,v in initial.items()]}}
receipt['protectedMetadata']={str(p):{'before':initial_metadata[str(p)],'after':file_identity(p) if p.exists() else None} for p in protected_paths}
receipt['protectedMetadataUnchanged']={str(p):p.exists() and file_identity(p)==initial_metadata[str(p)] for p in protected_paths}
receipt['sourceFunctionUnchanged']=(REPO/'crates/xcb-runtime/src/sandbox.rs').read_text().split('pub fn codex_seatbelt(',1)[1].split('\n/// Devin',1)[0]==function
(run/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
(ROOT/'latest-kernel-receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
required_labels={
    'scratch hard-link creation (observational)','config hard-link creation denied','catalog hard-link creation denied',
    'config symlink creation (observational)','config chmod denied','config rename denied',
    'config overwrite rename denied','profile rename denied','public CA read','public CA symlink read',
    'public CA write denied','public CA symlink write denied','public CA hard-link creation denied',
    'public CA chmod denied','public CA symlink chmod denied','public CA rename denied',
    'public CA overwrite rename denied','public CA unlink denied',
}
observed_labels={c['label'] for c in result.get('checks',[])}
failed=[c for c in result.get('checks',[]) if not c['matched']]
print(json.dumps({'receipt':str(run/'receipt.json'),'failures':failed,'result':result,'rootExitCode':p.returncode,'processGroupAbsent':absent}))
sys.exit(bool(failed) or p.returncode!=0 or not absent or not required_labels.issubset(observed_labels) or not all(receipt['protectedFilesUnchanged'].values()) or not all(receipt['protectedMetadataUnchanged'].values()) or not receipt['sourceFunctionUnchanged'])
