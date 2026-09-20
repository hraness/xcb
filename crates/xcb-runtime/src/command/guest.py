#!/usr/bin/python3
"""XCB trusted Linux command supervisor. Never installed in a consumer workspace."""
import base64
import contextlib
import ctypes
import socket
import struct
import fcntl
import hashlib
import json
import os
import pathlib
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import time
import tempfile

ROOT = pathlib.Path('/var/lib/xcb-command')
AGENT = '/usr/local/lib/xcb-command/guest.py'
UID = 61001
MAX_INPUT = 96 * 1024 * 1024
MAX_OUTPUT = 256 * 1024
HEX = re.compile(r'^[0-9a-f]{64}$')
IDENT = re.compile(r'^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,79}$')
# This pre-release helper wrote cgroup.json before every untrusted Popen.
# Its failed setup jobs can be reconciled only with that exact source hash.
LEGACY_PRELAUNCH_SHA = 'ef61affc5f1d6fe0833d765c0657ca57fd0013d9a67adc90669ff41eb90ad236'
ENV = {'PATH': '/opt/xcb-tools/bin:/usr/local/bin:/usr/bin:/bin', 'HOME': '/home/xcb',
       'LANG': 'C.UTF-8', 'TMPDIR': '/tmp', 'GIT_CONFIG_NOSYSTEM': '1',
       'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_TERMINAL_PROMPT': '0',
       'GIT_OPTIONAL_LOCKS': '0', 'CARGO_NET_OFFLINE': 'true', 'CARGO_BUILD_JOBS': '2', 'CARGO_INCREMENTAL': '0'}

def require(value, message):
    if not value:
        raise ValueError(message)

def digest(data):
    return hashlib.sha256(data).hexdigest()

def pairs(values):
    result = {}
    for key, value in values:
        require(key not in result, 'duplicate JSON field')
        result[key] = value
    return result

def decode(data):
    return json.loads(data, object_pairs_hook=pairs, parse_constant=lambda _: require(False, 'nonfinite JSON'))

def encode(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()

def closed(value, fields):
    require(isinstance(value, dict) and set(value) == set(fields), 'unknown or missing fields')

def excluded(path):
    blocked = {'.git', 'node_modules', 'target', 'dist', 'build', '.next', '.venv', 'venv', '__pycache__', '.cache', '.ssh', '.aws', '.gnupg', '.codex', '.claude', '.devin', '.npmrc', '.pypirc', '.netrc', '.DS_Store'}
    return any(name in blocked or name == '.env' or (name.startswith('.env.') and name not in ('.env.example', '.env.sample', '.env.template')) or name.startswith('.xcb-') for name in path.split('/'))

def relative(value, root=False):
    require(isinstance(value, str) and 0 < len(value.encode()) <= 4096, 'path length')
    if root and value == '.':
        return value
    require(all(p and p not in ('.', '..') for p in value.split('/')) and not value.startswith('/'), 'relative path')
    require(not any(ord(c) < 32 or ord(c) == 127 for c in value), 'path controls')
    require(not excluded(value), 'excluded command path')
    return value

def private_write(path, data):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        with os.fdopen(fd, 'wb') as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        d = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(d)
        finally:
            os.close(d)
    except BaseException:
        raise

def read_regular(path, limit):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as source:
        before = os.fstat(source.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1 and before.st_size <= limit, 'unsafe file')
        data = source.read(limit + 1)
        after = os.fstat(source.fileno())
        require(len(data) <= limit and (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
                (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns), 'changed file')
        return data

def boot_id():
    return pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()

def validate_request(request):
    closed(request, ('argv', 'cwd', 'timeoutMs', 'network'))
    argv = request['argv']
    require(isinstance(argv, list) and 1 <= len(argv) <= 64 and all(isinstance(a, str) and '\x00' not in a for a in argv), 'argv')
    require(argv[0] and sum(len(a.encode()) for a in argv) <= 32768, 'argv size')
    relative(request['cwd'], True)
    require(type(request['timeoutMs']) is int and 1 <= request['timeoutMs'] <= 600000, 'timeout')
    require(request['network'] == 'none', 'only offline commands are admitted')

def validate_snapshot(snapshot, workspace_id):
    require(isinstance(snapshot, dict) and set(snapshot) in ({'version', 'workspaceId', 'files', 'directories'}, {'version', 'workspaceId', 'files', 'directories', 'git'}), 'snapshot fields')
    if 'git' in snapshot:
        require(isinstance(snapshot['git'], dict), 'Git input envelope')
    require(snapshot['version'] == 1 and snapshot['workspaceId'] == workspace_id, 'snapshot binding')
    require(isinstance(snapshot['files'], list) and len(snapshot['files']) <= 8192, 'file count')
    require(isinstance(snapshot['directories'], list) and len(snapshot['directories']) <= 8192, 'directory count')
    names, files, total = set(), {}, 0
    for entry in snapshot['files']:
        closed(entry, ('path', 'base64', 'sha256', 'executable'))
        name = relative(entry['path'])
        require(name not in names and type(entry['executable']) is bool, 'duplicate path/mode')
        require(isinstance(entry['base64'], str) and len(entry['base64']) <= 3 * 1024 * 1024, 'encoded file size')
        data = base64.b64decode(entry['base64'], validate=True)
        require(len(data) <= 2 * 1024 * 1024 and digest(data) == entry['sha256'], 'file digest/size')
        names.add(name)
        files[name] = (data, entry['executable'])
        total += len(data)
    require(total <= 64 * 1024 * 1024, 'snapshot size')
    directories = set()
    for name in snapshot['directories']:
        relative(name)
        require(name not in names and name not in directories, 'duplicate directory')
        directories.add(name)
    for name in names | directories:
        parts = name.split('/')
        require(all('/'.join(parts[:i]) not in names for i in range(1, len(parts))), 'file ancestor')
    return files, directories

def job_path(identifier):
    require(isinstance(identifier, str) and IDENT.fullmatch(identifier), 'command ID')
    return ROOT / 'jobs' / identifier

def tool_identities():
    names = {'python': '/usr/bin/python3', 'git': '/usr/bin/git', 'sh': '/usr/bin/dash',
             'cc': '/usr/bin/cc', 'bun': '/opt/xcb-tools/bin/bun'}
    for name in ('node', 'rustc', 'cargo'):
        preferred = pathlib.Path('/opt/xcb-tools/bin') / name
        names[name] = str(preferred if preferred.exists() else pathlib.Path('/usr/bin') / name)
    identities = {}
    for name, selected in names.items():
        path = pathlib.Path(selected).resolve(strict=True)
        metadata = path.stat()
        require(metadata.st_uid == 0 and metadata.st_mode & 0o022 == 0 and metadata.st_mode & 0o111,
                'tool executable permissions')
        data = read_regular(path, 256 * 1024 * 1024)
        require(pathlib.Path(selected).resolve(strict=True) == path, 'tool pathname changed')
        identities[name] = {'path': str(path), 'sha256': digest(data)}
    return identities

def inspect():
    require(os.geteuid() == 0, 'trusted supervisor requires root')
    require(pathlib.Path('/sys/fs/cgroup/cgroup.controllers').is_file(), 'cgroup v2 required')
    bwrap = subprocess.run(['/usr/bin/bwrap', '--help'], capture_output=True, check=True).stdout
    require(b'--disable-userns' in bwrap and b'--unshare-all' in bwrap, 'bubblewrap features')
    return {'version': 1, 'bootId': boot_id(), 'agentSha256': digest(read_regular(pathlib.Path(AGENT), 128 * 1024)),
            'bwrapSha256': digest(read_regular(pathlib.Path('/usr/bin/bwrap'), 2 * 1024 * 1024)),
            'toolDigests': tool_identities(),
            'gitProjectionSha256': digest(read_regular(pathlib.Path(AGENT).with_name('git_projection.py'), 128 * 1024)),
            'publicCacheSha256': digest(read_regular(pathlib.Path(AGENT).with_name('public_cache.py'), 128 * 1024))}

def receive():
    # Root-only global custody is outside all worker namespaces. The lock is
    # never inherited by workers; if SSH dies, durable unfinished jobs below
    # continue to block admission until independently joined recovery exists.
    fd = os.open(ROOT / 'admission.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        metadata = os.fstat(fd)
        require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == 0 and metadata.st_nlink == 1
                and metadata.st_mode & 0o077 == 0, 'unsafe admission lock')
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        cache_pending()
        for previous in (ROOT / 'jobs').iterdir():
            require(previous.is_dir() and not previous.is_symlink(), 'invalid prior command')
            result = decode(read_regular(previous / 'result.json', 36 * 1024 * 1024))
            require(result.get('joined') is True, 'prior command custody remains unjoined')
        return receive_owned()
    finally:
        os.close(fd)

def receive_owned():
    data = sys.stdin.buffer.read(140 * 1024 * 1024 + 1)
    require(len(data) <= 140 * 1024 * 1024, 'input envelope limit')
    envelope = decode(data)
    closed(envelope, ('custody', 'request', 'snapshotBase64'))
    custody = envelope['custody']
    closed(custody, ('version', 'commandId', 'runId', 'workspaceId', 'snapshotSha256', 'requestSha256', 'backendSha256', 'bootId'))
    require(custody['version'] == 1 and custody['bootId'] == boot_id(), 'guest boot changed')
    require(all(isinstance(custody[k], str) and HEX.fullmatch(custody[k]) for k in ('workspaceId', 'snapshotSha256', 'requestSha256', 'backendSha256')), 'digest binding')
    require(IDENT.fullmatch(custody['runId']), 'run ID')
    validate_request(envelope['request'])
    require(digest(encode(envelope['request'])) == custody['requestSha256'], 'request digest')
    seed = base64.b64decode(envelope['snapshotBase64'], validate=True)
    require(len(seed) <= MAX_INPUT and digest(seed) == custody['snapshotSha256'], 'snapshot digest')
    files, directories = validate_snapshot(decode(seed), custody['workspaceId'])
    job = job_path(custody['commandId'])
    require(sum(not (entry / 'ack.json').exists() for entry in (ROOT / 'jobs').iterdir()) < 64, 'unacknowledged command limit')
    retained = sum(f.stat().st_size for f in (ROOT / 'jobs').glob('*/*.json') if f.is_file())
    retained_entries = 0
    for projection in (ROOT / 'jobs').glob('*/git-projection'):
        require(projection.is_dir() and not projection.is_symlink(), 'retained projection directory')
        for parent, dirs, names in os.walk(projection, followlinks=False):
            retained_entries += len(dirs) + len(names)
            require(retained_entries <= 100000, 'retained projection entry limit')
            for name in dirs + names:
                retained += os.lstat(pathlib.Path(parent) / name).st_size
                require(retained <= 512 * 1024 * 1024, 'retained projection byte limit')
    require(retained + len(seed) < 512 * 1024 * 1024, 'retained command byte limit')
    # Execute-only ancestors let the trusted, already UID-dropped bwrap
    # resolve its selected bind source. Control records stay root-only0600;
    # none of these ancestors exists inside the worker mount namespace.
    ROOT.chmod(0o711)
    (ROOT / 'jobs').chmod(0o711)
    job.mkdir(mode=0o711)
    private_write(job / 'custody.json', encode(custody))
    private_write(job / 'request.json', encode(envelope['request']))
    private_write(job / 'seed.json', seed)
    private_write(job / 'launch.json', encode({'version': 1, 'guestSha256': digest(read_regular(pathlib.Path(AGENT),128*1024)), 'custody': custody}))
    work = job / 'work'
    work.mkdir(mode=0o700)
    space = os.statvfs(ROOT)
    require(space.f_bavail * space.f_frsize >= 2304 * 1024 * 1024, 'insufficient isolated build scratch space')
    image = job / 'work.img'
    private_write(image, b'')
    os.truncate(image, 2 * 1024 * 1024 * 1024)
    subprocess.run(['/usr/sbin/mkfs.ext4', '-q', '-F', '-E', 'lazy_itable_init=1,lazy_journal_init=1', str(image)], check=True)
    subprocess.run(['/usr/bin/mount', '-o', 'loop,nodev,nosuid,discard', str(image), str(work)], check=True)
    for name in sorted(directories, key=lambda x: (x.count('/'), x)):
        (work / name).mkdir(mode=0o700, parents=True, exist_ok=True)
    for name, (contents, executable) in files.items():
        path = work / name
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        private_write(path, contents)
        path.chmod(0o700 if executable else 0o600)
    for parent, dirs, names in os.walk(work):
        os.chown(parent, UID, UID)
        for name in names:
            os.chown(pathlib.Path(parent) / name, UID, UID)
    # No directory FD is inherited into the untrusted process.
    unit = 'xcb-command-' + custody['commandId']
    seconds = (envelope['request']['timeoutMs'] + 999) // 1000 + 180
    command = ['/usr/bin/systemd-run', '--quiet', '--wait', '--pipe', '--unit=' + unit,
               '--property=Delegate=yes', '--property=KillMode=control-group',
               '--property=TasksMax=256', '--property=MemoryMax=2G',
               '--property=RuntimeMaxSec=' + str(seconds), '--property=TimeoutStopSec=8',
               '/usr/bin/python3', AGENT, 'supervise', custody['commandId']]
    # systemd owns the actual job independently of this SSH transport.
    completed = subprocess.run(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                               timeout=seconds + 15, check=False)
    del completed
    if not (job / 'result.json').exists():
        return reconcile_prelaunch(custody, digest(read_regular(pathlib.Path(AGENT),128*1024)))
    return status(custody['commandId'])

def reconcile_prelaunch(custody, expected_source):
    job = job_path(custody['commandId'])
    require(decode(read_regular(job / 'custody.json',8192)) == custody and custody['bootId'] == boot_id(), 'prelaunch custody')
    result={'version':1,'custody':custody,'joined':True,'unstarted':True,'cgroup':None,'exitCode':None,
            'stdout':'','stderr':'','timedOut':False,'cancelled':False,'truncated':False,
            'error':'command did not start: guest preparation failed', 'changesBase64':None,'changesSha256':None}
    proof_path=job / 'prelaunch-recovery.json'
    if proof_path.exists():
        proof=decode(read_regular(proof_path,16384))
        closed(proof,('version','guestSha256','custody','service'))
        require(proof['version']==1 and proof['custody']==custody and proof['guestSha256']==expected_source,
                'prelaunch recovery proof changed')
        require(proof['service'].get('ActiveState') in ('failed','inactive') and proof['service'].get('MainPID')=='0'
                and proof['service'].get('ControlGroup')=='', 'prelaunch original exit evidence')
        if (job / 'result.json').exists():
            require(decode(read_regular(job / 'result.json',16384))==result, 'prelaunch result changed')
            if os.path.ismount(job / 'work'):
                subprocess.run(['/usr/bin/umount',str(job / 'work')],check=True)
            return result
    require(digest(read_regular(job / 'seed.json',MAX_INPUT)) == custody['snapshotSha256'], 'prelaunch seed changed')
    request=decode(read_regular(job / 'request.json',65536))
    require(digest(encode(request)) == custody['requestSha256'], 'prelaunch request changed')
    launch = job / 'launch.json'
    if launch.exists():
        record=decode(read_regular(launch,16384))
        closed(record,('version','guestSha256','custody'))
        require(record['version'] == 1 and record['custody'] == custody and record['guestSha256'] == expected_source,
                'prelaunch source binding')
        require(not (job / 'child-attempted.json').exists(), 'untrusted launch may have started')
    else:
        require(expected_source == LEGACY_PRELAUNCH_SHA and not (job / 'cgroup.json').exists(), 'legacy prelaunch evidence missing')
    unit='xcb-command-'+custody['commandId']+'.service'
    observation=subprocess.run(['/usr/bin/systemctl','show',unit,'--property=ActiveState,SubState,MainPID,ControlGroup'],capture_output=True,check=True,timeout=5).stdout.decode()
    state=dict(line.split('=',1) for line in observation.splitlines())
    require(state['ActiveState'] in ('failed','inactive') and state['MainPID']=='0' and not state['ControlGroup'], 'original supervisor still active')
    require(not pathlib.Path('/sys/fs/cgroup/system.slice/'+unit).exists(), 'original service cgroup still exists')
    if not proof_path.exists():
        private_write(proof_path,encode({'version':1,'guestSha256':expected_source,'custody':custody,'service':state}))
    private_write(job / 'result.json',encode(result))
    if os.path.ismount(job / 'work'):
        subprocess.run(['/usr/bin/umount',str(job / 'work')],check=True)
    return result

def changes(job, workspace_id):
    seed, _ = validate_snapshot(decode(read_regular(job / 'seed.json', MAX_INPUT)), workspace_id)
    current, total = {}, 0
    work = job / 'work'
    count = 0
    for parent, dirs, names in os.walk(work, followlinks=False):
        relative_parent = pathlib.Path(parent).relative_to(work)
        dirs[:] = [name for name in dirs if not excluded((relative_parent / name).as_posix())]
        names = [name for name in names if not excluded((relative_parent / name).as_posix())]
        require(len(relative_parent.parts) <= 64, 'output depth limit')
        count += len(dirs)
        require(count <= 8192, 'output entry limit')
        for name in dirs:
            require(stat.S_ISDIR(os.lstat(pathlib.Path(parent) / name).st_mode), 'output directory symlink')
        for name in names:
            path = pathlib.Path(parent) / name
            rel = relative(path.relative_to(work).as_posix())
            count += 1
            require(count <= 8192, 'output file count')
            data = read_regular(path, 2 * 1024 * 1024)
            executable = bool(os.lstat(path).st_mode & 0o111)
            current[rel] = (data, executable)
            total += len(data)
            require(total <= 256 * 1024 * 1024, 'output workspace limit')
    result, size = [], 0
    for name in sorted(set(seed) | set(current)):
        if seed.get(name) == current.get(name):
            continue
        present = current.get(name)
        data, executable = present if present else (None, False)
        size += len(data) if data is not None else 0
        require(size <= 16 * 1024 * 1024 and len(result) < 512, 'change limit')
        result.append({'path': name, 'base64': base64.b64encode(data).decode() if data is not None else None,
                       'sha256': digest(data) if data is not None else None, 'executable': executable})
    payload = encode({'version': 1, 'workspaceId': workspace_id, 'changes': result})
    require(len(payload) <= 24 * 1024 * 1024, 'changes JSON limit')
    return payload

def populated(cgroup):
    text = (cgroup / 'cgroup.events').read_text()
    entries = dict(line.split() for line in text.splitlines())
    require(entries.get('populated') in ('0', '1'), 'invalid cgroup observation')
    return entries['populated'] == '1'

def sandbox_argv(work, request, projected=None, cache=None):
    # The child has already dropped to the dedicated uid before bwrap creates
    # its namespaces. No host filesystem or directory FD is inherited.
    command = ['/usr/bin/bwrap', '--unshare-all', '--unshare-user', '--die-with-parent', '--new-session',
               '--disable-userns', '--cap-drop', 'ALL',
               '--ro-bind', '/usr', '/usr', '--symlink', 'usr/bin', '/bin',
               '--symlink', 'usr/sbin', '/sbin', '--symlink', 'usr/lib', '/lib',
               '--ro-bind', '/opt/xcb-tools', '/opt/xcb-tools', '--proc', '/proc', '--dev', '/dev',
               '--tmpfs', '/tmp', '--tmpfs', '/home', '--dir', '/home/xcb',
               '--dir', '/etc', '--ro-bind', '/etc/ld.so.cache', '/etc/ld.so.cache',
               '--ro-bind', '/etc/alternatives', '/etc/alternatives',
               '--bind', str(work), '/work', '--chdir', '/work' + ('' if request['cwd'] == '.' else '/' + request['cwd']),
               '--clearenv']
    if projected is not None:
        command += ['--ro-bind', str(projected / '.git'), '/work/.git']
    if cache is not None:
        command += ['--ro-bind', str(cache), '/opt/xcb-cache']
        if (cache / 'cargo/config.toml').is_file():
            command += ['--dir', '/home/xcb/.cargo', '--ro-bind', str(cache / 'cargo/config.toml'), '/home/xcb/.cargo/config.toml']
        if (cache / 'bun').is_dir():
            command += ['--setenv', 'BUN_INSTALL_CACHE_DIR', '/opt/xcb-cache/bun']
    for key, value in ENV.items():
        command += ['--setenv', key, value]
    return command + ['--'] + request['argv']

@contextlib.contextmanager
def readonly_phase_input(value):
    # The writable staging descriptor is never inherited by a parser. A read
    # descriptor cannot grow an anonymous input outside its output quota.
    with tempfile.TemporaryFile() as staging:
        staging.write(encode(value));staging.flush();staging.seek(0)
        fd=os.open('/proc/self/fd/'+str(staging.fileno()),os.O_RDONLY|os.O_CLOEXEC)
        with os.fdopen(fd,'rb') as source:
            yield source


def run_phase(identifier, cgroup, argv, timeout_ms, stop, input_file=None, owner=None, max_output=MAX_OUTPUT, private_loopback=False):
    owner = owner or job_path(identifier)
    def enter():
        with open(cgroup / 'cgroup.procs', 'w') as target:
            target.write(str(os.getpid()))
        if private_loopback:
            libc = ctypes.CDLL(None, use_errno=True)
            if libc.unshare(0x40000000) != 0:
                raise OSError(ctypes.get_errno(), 'private network namespace')
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as control:
                flags = fcntl.ioctl(control.fileno(), 0x8913, struct.pack('256s', b'lo'))
                current = struct.unpack('H', flags[16:18])[0]
                fcntl.ioctl(control.fileno(), 0x8914, struct.pack('16sH238x', b'lo', current | 1))
        os.setgroups([])
        os.setgid(UID)
        os.setuid(UID)
    child = None
    joined = False
    out, err = bytearray(), bytearray()
    truncated = timed_out = cancelled = False
    problem = None
    class CancelledBeforeLaunch(Exception):
        pass
    try:
        if stop[0] or (ROOT / 'cancel' / identifier).exists():
            joined = not populated(cgroup)
            cancelled = True
            raise CancelledBeforeLaunch()
        # One durable marker covers any untrusted phase. Reuse requires the
        # same exact custody, never removal between projection and execution.
        marker = owner / 'child-attempted.json'
        expected = {'version': 1, 'custody': decode(read_regular(owner / 'custody.json', 32768))}
        if marker.exists():
            require(decode(read_regular(marker, 16384)) == expected, 'launch marker changed')
        else:
            private_write(marker, encode(expected))
        child = subprocess.Popen(argv, stdin=input_file if input_file is not None else subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=ENV,
                                 preexec_fn=enter, close_fds=True)
        with selectors.DefaultSelector() as selector:
            for stream, buffer in ((child.stdout, out), (child.stderr, err)):
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, buffer)
            deadline = time.monotonic() + timeout_ms / 1000
            terminating = False
            drain_deadline = None
            while True:
                cancelled = cancelled or stop[0] or (ROOT / 'cancel' / identifier).exists()
                timed_out = timed_out or time.monotonic() >= deadline
                if (cancelled or timed_out or truncated or child.poll() is not None) and not terminating:
                    (cgroup / 'cgroup.kill').write_text('1')
                    terminating = True
                    drain_deadline = time.monotonic() + 5
                for key, _ in selector.select(0.02):
                    chunk = os.read(key.fd, 65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                    else:
                        capacity = max_output - len(out) - len(err)
                        key.data.extend(chunk[:max(0, capacity)])
                        truncated = truncated or len(chunk) > capacity
                if terminating and not populated(cgroup) and not selector.get_map():
                    child.wait(timeout=1)
                    joined = True
                    break
                if drain_deadline and time.monotonic() >= drain_deadline:
                    raise RuntimeError('descendants or output streams did not join')
    except CancelledBeforeLaunch:
        pass
    except BaseException as error:
        problem = type(error).__name__ + ': command supervision failed'
    finally:
        if not joined:
            try:
                (cgroup / 'cgroup.kill').write_text('1')
                until = time.monotonic() + 5
                while populated(cgroup) and time.monotonic() < until:
                    time.sleep(0.02)
                if child is not None:
                    child.wait(timeout=1)
            except BaseException:
                pass
        if child is not None:
            child.stdout.close()
            child.stderr.close()
    return {'joined': joined, 'exitCode': child.returncode if child else None,
            'stdout': out.decode('utf-8', 'replace'), 'stderr': err.decode('utf-8', 'replace'),
            'timedOut': timed_out, 'cancelled': cancelled, 'truncated': truncated, 'error': problem}


def prepare_cgroup(identifier, unit_prefix='xcb-command-'):
    control = pathlib.Path('/proc/self/cgroup').read_text().strip().split('0::', 1)[1]
    require(control == '/system.slice/' + unit_prefix + identifier + '.service', 'unit identity')
    service = pathlib.Path('/sys/fs/cgroup' + control)
    supervisor = service / 'supervisor'
    supervisor.mkdir()
    (supervisor / 'cgroup.procs').write_text(str(os.getpid()))
    require(not (service / 'cgroup.procs').read_text().strip(), 'service root has another process')
    (service / 'cgroup.subtree_control').write_text('+memory +pids')
    cgroup = service / 'worker'
    cgroup.mkdir()
    # The supervisor remains outside this constrained child. OOM kills all
    # untrusted descendants together and leaves room to record their join.
    (cgroup / 'memory.max').write_text(str(1536 * 1024 * 1024))
    (cgroup / 'memory.swap.max').write_text('0')
    (cgroup / 'memory.oom.group').write_text('1')
    identity = os.stat(cgroup)
    control_receipt = {'path': str(cgroup), 'dev': identity.st_dev, 'ino': identity.st_ino}
    return cgroup, control_receipt

def supervise(identifier):
    require(os.geteuid() == 0, 'trusted root supervisor required')
    job = job_path(identifier)
    custody = decode(read_regular(job / 'custody.json', 8192))
    request = decode(read_regular(job / 'request.json', 65536))
    validate_request(request)
    require(custody['bootId'] == boot_id(), 'guest restarted')
    cgroup, control_receipt = prepare_cgroup(identifier)
    private_write(job / 'cgroup.json', encode(control_receipt))
    stop = [False]
    def interrupted(_signum, _frame):
        stop[0] = True
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    git_snapshot = decode(read_regular(job / 'seed.json', MAX_INPUT)).get('git')
    projected = None
    result = None
    if git_snapshot is not None:
        import git_projection
        projected = job / 'git-projection'
        projected.mkdir(mode=0o700)
        subprocess.run(['/usr/bin/mount', '-t', 'tmpfs', '-o', 'size=80M,mode=0700,nodev,nosuid', 'xcb-git-projection', str(projected)], check=True)
        os.chown(projected, UID, UID)
        # The root supervisor never interprets Git objects. Only the separate
        # no-network, UID-dropped namespace receives these raw input bytes.
        with readonly_phase_input(git_snapshot) as input_file:
            phase = run_phase(identifier, cgroup, git_projection.sandbox_argv(projected),
                              60000, stop, input_file)
        private_write(job / 'projection-phase.json', encode(phase))
        if phase['joined'] and phase['exitCode'] == 0 and not any(phase[name] for name in ('timedOut', 'cancelled', 'truncated', 'error')):
            try:
                receipt = decode(phase['stdout'].encode())
                closed(receipt, ('version', 'syntheticHead', 'headFiles', 'indexFiles', 'projectionSha256'))
                require(receipt['version'] == 1 and receipt['syntheticHead'] is True
                        and receipt['projectionSha256'] == git_projection.validate_output(projected), 'Git projection binding')
                private_write(job / 'projection.json', encode(receipt))
            except BaseException:
                result = dict(phase, stdout='', stderr='', error='Git projection output failed validation')
        else:
            result = dict(phase, stdout='', stderr='', error='Git projection failed; command did not start')
    cache = None
    cache_notice = ''
    if result is None:
        import public_cache
        snapshot = decode(read_regular(job / 'seed.json', MAX_INPUT))
        try:
            spec = cache_spec_from_snapshot(snapshot, tool_identities())
        except ValueError:
            spec = None
            cache_notice = 'XCB: dependency manifest limits exceeded. Command remains offline.\n'
        if spec is not None:
            plan_root = job / 'cache-plan'
            plan_root.mkdir(mode=0o700)
            subprocess.run(['/usr/bin/mount','-t','tmpfs','-o','size=32M,mode=0711,nodev,nosuid','xcb-cache-plan',str(plan_root)],check=True)
            try:
                phase, _ = cache_phase(job, identifier, cgroup, stop, 'plan', spec, plan_root)
                if not phase['joined'] or any(phase[name] for name in ('cancelled','timedOut','truncated')):
                    result = dict(phase, stdout='', stderr='', error='dependency cache planning did not complete')
                elif phase_success(phase):
                    try:
                        plan = public_cache.validate_plan(decode(phase['stdout'].encode()))
                        require(plan['toolDigests']==spec['toolDigests'] and plan['inputs']==[{'path':row['path'],'sha256':row['sha256']} for row in spec['files']], 'command cache input binding')
                        verified_cache(plan)
                        record = decode(read_regular(ROOT / 'cache' / plan['cacheKey'] / 'record.json', 4*1024*1024))
                        cache = ROOT / 'preloads' / record['attemptId'] / 'work/materialize-output'
                        private_write(job / 'cache-selection.json', encode({'version':1,'cacheKey':plan['cacheKey'],'inputs':plan['inputs']}))
                    except (OSError, ValueError):
                        cache_notice = 'XCB: no prepared dependency cache matches these exact lockfiles and tools. Command remains offline.\n'
                else:
                    cache_notice = 'XCB: dependency inputs are unsupported or unprepared. Command remains offline.\n'
            finally:
                if not populated(cgroup):
                    subprocess.run(['/usr/bin/umount',str(plan_root)],check=True)
    if result is None:
        result = run_phase(identifier, cgroup, sandbox_argv(job / 'work', request, projected, cache),
                           request['timeoutMs'], stop)
        if cache_notice:
            available = MAX_OUTPUT - len(result['stdout'].encode()) - len(result['stderr'].encode())
            result['stderr'] = cache_notice[:max(0,available)] + result['stderr']
    delta = None
    if result['joined']:
        try:
            delta = changes(job, custody['workspaceId'])
            private_write(job / 'changes.json', delta)
        except BaseException:
            result['error'] = 'command output includes unsupported paths/files or exceeds the bounded change limit'
    result.update({'version': 1, 'custody': custody, 'cgroup': control_receipt, 'unstarted': False,
                   'changesBase64': base64.b64encode(delta).decode() if delta is not None else None,
                   'changesSha256': digest(delta) if delta is not None else None})
    private_write(job / 'result.json', encode(result))
    if result['joined']:
        subprocess.run(['/usr/bin/umount', str(job / 'work')], check=True)
    return None

def status(identifier):
    job = job_path(identifier)
    result = decode(read_regular(job / 'result.json', 36 * 1024 * 1024))
    require(result['custody']['commandId'] == identifier and result['custody']['bootId'] == boot_id(), 'receipt binding')
    return result

def acknowledge(identifier):
    data = sys.stdin.buffer.read(16385)
    require(len(data) <= 16384, 'acknowledgment size')
    ack = decode(data)
    closed(ack, ('version', 'custody', 'resultSha256', 'hostReceiptSha256'))
    require(ack['version'] == 1 and HEX.fullmatch(ack['hostReceiptSha256']), 'host acknowledgment')
    job = job_path(identifier)
    result = decode(read_regular(job / 'result.json', 36 * 1024 * 1024))
    require(result['custody'] == ack['custody'] and result['custody']['commandId'] == identifier
            and result['joined'] is True and digest(encode(result) + b'\n') == ack['resultSha256'], 'acknowledgment custody')
    control = result['cgroup']
    expected = '/sys/fs/cgroup/system.slice/xcb-command-' + identifier + '.service/worker'
    if result.get('unstarted') is True:
        proof=decode(read_regular(job / 'prelaunch-recovery.json',16384))
        require(control is None and proof['custody']==ack['custody'], 'missing no-start evidence')
    else:
        require(control['path'] == expected, 'acknowledgment cgroup path')
        cgroup = pathlib.Path(expected)
        try:
            observed = cgroup.stat()
        except FileNotFoundError:
            pass  # The durable result already proved joined; absence is not proof by itself.
        else:
            require((observed.st_dev, observed.st_ino) == (control['dev'], control['ino']) and not populated(cgroup), 'command cgroup still active or replaced')
    work = job / 'work'
    require(not os.path.ismount(work), 'scratch is still mounted')
    prior = job / 'ack.json'
    if prior.exists():
        # Different durable host receipts may contain the same exact result.
        recorded = decode(read_regular(prior,16384))
        require(recorded['custody'] == ack['custody'] and recorded['resultSha256'] == ack['resultSha256'], 'acknowledgment changed')
    else:
        private_write(prior, encode(ack))
    seed = job / 'seed.json'
    if seed.exists():
        require(digest(read_regular(seed,MAX_INPUT)) == ack['custody']['snapshotSha256'], 'acknowledged seed changed')
        seed.unlink()
    image = job / 'work.img'
    if image.exists():
        metadata = image.lstat()
        require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == 0 and metadata.st_nlink == 1 and metadata.st_size == 2 * 1024 * 1024 * 1024, 'scratch image identity')
        image.unlink()
    if work.exists():
        require(work.is_dir() and not work.is_symlink(), 'scratch directory changed')
        work.rmdir()
    projection = job / 'git-projection'
    if projection.exists():
        # This exact task directory is reproducible. All untrusted phases and
        # the final worker have joined; no descendant can mutate it now.
        require(projection.is_dir() and not projection.is_symlink(), 'projection directory changed')
        observed = projection.stat()
        if observed.st_uid == 0 and not os.path.ismount(projection) and not any(projection.iterdir()):
            pass  # A retry after the previous exact unmount, before rmdir.
        else:
            require(observed.st_uid == UID, 'projection owner changed')
            if (job / 'projection.json').exists():
                import git_projection
                recorded = decode(read_regular(job / 'projection.json', 8192))
                require(git_projection.validate_output(projection) == recorded['projectionSha256'], 'acknowledged projection changed')
            if os.path.ismount(projection):
                subprocess.run(['/usr/bin/umount', str(projection)], check=True)
        require(shutil.rmtree.avoids_symlink_attacks, 'safe projection cleanup unavailable')
        shutil.rmtree(projection)
    directory = os.open(job,os.O_RDONLY|os.O_DIRECTORY)
    try:os.fsync(directory)
    finally:os.close(directory)
    return {'acknowledged': True}

# Trusted cache orchestration: no consumer parser executes as root.
def cache_mount(job):
    work = job / 'work'
    work.mkdir(mode=0o711)
    image = job / 'work.img'
    private_write(image, b'')
    os.truncate(image, 1024 * 1024 * 1024)
    subprocess.run(['/usr/sbin/mkfs.ext4','-q','-F','-E','lazy_itable_init=1,lazy_journal_init=1',str(image)], check=True)
    subprocess.run(['/usr/bin/mount','-o','loop,nodev,nosuid,discard',str(image),str(work)],check=True)
    work.chmod(0o711)
    return work

def phase_directory(work, name):
    path=work/name
    path.mkdir(mode=0o700)
    os.chown(path,UID,UID)
    return path

def cache_pending():
    directory=ROOT/'preloads'
    if not directory.exists():return
    rows=list(directory.iterdir())
    require(len(rows)<4096,'preload evidence count limit')
    for job in rows:
        require(job.is_dir() and not job.is_symlink(),'preload evidence directory')
        result=job/'recovered.json' if (job/'recovered.json').exists() else job/'result.json'
        if not result.exists() and decode(read_regular(job/'custody.json',32768))['operation']=='public-cache-plan':
            cache_recover_attempt(job)
        require((result.exists() or (job/'recovered.json').exists()) and cache_outcome(job)['joined'] is True,
                'dependency preparation custody remains pending; inspect/recover it first')

def cache_spec_from_snapshot(snapshot, tools):
    files={entry['path']:entry for entry in snapshot['files']}
    cargo='Cargo.lock' in files and 'Cargo.toml' in files
    bun='bun.lock' in files and 'package.json' in files
    if not cargo and not bun:return None
    names=({'Cargo.toml'} if cargo else set()) | ({'package.json'} if bun else set())
    selected=[]
    for path,entry in sorted(files.items()):
        if pathlib.Path(path).name in names or path in (('Cargo.lock',) if cargo else ()) + (('bun.lock',) if bun else ()):
            selected.append({k:entry[k] for k in ('path','base64','sha256')})
    require(len(selected)<=256 and sum(len(base64.b64decode(row['base64'],validate=True)) for row in selected)<=8*1024*1024,'dependency manifest limit')
    return {'version':1,'toolDigests':{name:tools[name]['sha256'] for name in ('bun','cargo','git','node','python','rustc')},'files':selected}

def cache_identity():
    observation=inspect()
    return {key:observation[key] for key in ('bootId','agentSha256','bwrapSha256','toolDigests','publicCacheSha256')}

def cache_operation(operation):
    import uuid
    payload=sys.stdin.buffer.read(16*1024*1024+1)
    require(len(payload)<=16*1024*1024,'cache request bound')
    value=decode(payload)
    expected=None
    if operation=='public-cache-prepare':
        closed(value,('version','expectedCacheKey','spec'))
        require(value['version']==1 and HEX.fullmatch(value['expectedCacheKey']),'cache key envelope')
        expected=value['expectedCacheKey'];spec=value['spec']
    else:spec=value
    closed(spec,('version','toolDigests','files'))
    identity=cache_identity()
    require(spec['toolDigests']=={name:identity['toolDigests'][name]['sha256'] for name in ('bun','cargo','git','node','python','rustc')},'cache tools changed')
    fd=os.open(ROOT/'admission.lock',os.O_RDWR|os.O_NOFOLLOW)
    try:
        fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
        cache_pending()
        for previous in (ROOT/'jobs').iterdir():
            require(decode(read_regular(previous/'result.json',36*1024*1024)).get('joined') is True,'prior command remains active')
        space=os.statvfs(ROOT)
        require(space.f_bavail*space.f_frsize>=1280*1024*1024,'insufficient bounded dependency scratch')
        preload=ROOT/'preloads';preload.mkdir(mode=0o711,exist_ok=True)
        identifier='cache_'+uuid.uuid4().hex
        job=preload/identifier;job.mkdir(mode=0o711)
        custody={'version':1,'attemptId':identifier,'operation':operation,'expectedCacheKey':expected,
                 'requestSha256':digest(encode(spec)),'identity':identity}
        private_write(job/'custody.json',encode(custody));private_write(job/'input.json',encode(spec))
        unit='xcb-preload-'+identifier
        result=subprocess.run(['/usr/bin/systemd-run','--quiet','--wait','--pipe','--unit='+unit,
             '--property=Delegate=yes','--property=KillMode=control-group','--property=TasksMax=256',
             '--property=MemoryMax=2G','--property=RuntimeMaxSec=1230','--property=TimeoutStopSec=8',
             '/usr/bin/python3',AGENT,'public-cache-supervise',identifier],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=1245,check=False)
        del result
        outcome=cache_outcome(job)
        require(outcome['custody']==custody and outcome['joined'] is True,'cache supervision did not join')
        if operation=='public-cache-plan':
            require(outcome['error'] is None and outcome['plan'] is not None,'cache plan refused')
            return outcome['plan']
        require(outcome['error'] is None and outcome['receipt'] is not None,'cache preparation failed; inspect retained attempt')
        require(verified_cache(outcome['plan'])==outcome['receipt'],'cache publication incomplete; recover retained preparation')
        return outcome['receipt']
    finally:os.close(fd)

def cache_phase(job,identifier,cgroup,stop,phase,spec,work,fetched=None):
    import public_cache
    output=phase_directory(work,phase+'-output')
    scratch=phase_directory(work,phase+'-scratch')
    argv=public_cache.sandbox_argv(output,scratch,phase,fetched=fetched,private_loopback=phase=='materialize')
    with readonly_phase_input(spec) as input_file:
        result=run_phase(identifier,cgroup,argv,60000 if phase=='plan' else 570000,stop,input_file,
                         owner=job,max_output=4*1024*1024,private_loopback=phase=='materialize')
    private_write(job/(phase+'-phase.json'),encode(result))
    return result,output

def phase_success(result):
    return result['joined'] and result['exitCode']==0 and not any(result[name] for name in ('error','timedOut','truncated','cancelled'))

def cache_supervise(identifier):
    import public_cache
    require(IDENT.fullmatch(identifier) and identifier.startswith('cache_'),'preload attempt')
    job=ROOT/'preloads'/identifier
    custody=decode(read_regular(job/'custody.json',32768))
    require(custody['identity']==cache_identity(),'preload environment changed')
    spec=decode(read_regular(job/'input.json',16*1024*1024))
    require(digest(encode(spec))==custody['requestSha256'],'preload input changed')
    cgroup,control_receipt=prepare_cgroup(identifier,'xcb-preload-')
    private_write(job/'cgroup.json',encode(control_receipt))
    private_write(job/'service-started.json',encode({'version':1,'custody':custody,'control':control_receipt}))
    stop=[False]
    def stopped(_signum,_frame):stop[0]=True
    signal.signal(signal.SIGTERM,stopped);signal.signal(signal.SIGINT,stopped)
    work=None;joined=True;plan=None;receipt=None;problem=None;published=False;publication=None
    try:
        work=cache_mount(job)
        joined=False
        phase,output=cache_phase(job,identifier,cgroup,stop,'plan',spec,work)
        joined=phase['joined'];require(phase_success(phase),'cache plan failed')
        plan=public_cache.validate_plan(decode(phase['stdout'].encode()))
        require(plan['toolDigests']==spec['toolDigests'] and plan['inputs']==[{'path':row['path'],'sha256':row['sha256']} for row in spec['files']],'cache plan input binding')
        private_write(job/'plan.json',encode(plan))
        if custody['operation']=='public-cache-prepare':
            require(plan['cacheKey']==custody['expectedCacheKey'],'cache plan key changed')
            cache=ROOT/'cache'/plan['cacheKey']
            if cache.exists():
                receipt=verified_cache(plan,remount=True)
            else:
                joined=False
                phase,fetched=cache_phase(job,identifier,cgroup,stop,'fetch',plan,work)
                joined=phase['joined'];require(phase_success(phase),'cache fetch failed')
                fetch_receipt=decode(phase['stdout'].encode())
                require(fetch_receipt=={'version':1,'cacheKey':plan['cacheKey'],'fetched':True,'inventory':public_cache.inventory(fetched)},'fetched cache inventory mismatch')
                joined=False
                phase,output=cache_phase(job,identifier,cgroup,stop,'materialize',spec,work,fetched)
                joined=phase['joined'];require(phase_success(phase),'cache materialization failed')
                inventory=public_cache.inventory(output,allow_links=True)
                expected={'version':1,'cacheKey':plan['cacheKey'],'complete':True,'inventory':inventory}
                require(decode(phase['stdout'].encode())==expected,'materialized cache inventory mismatch')
                # No parser/Git/package child remains. Freeze every output inode
                # before publishing its readonly namespace to ordinary workers.
                for parent,dirs,names in os.walk(output,followlinks=False):
                    os.chown(parent,0,0);os.chmod(parent,0o555)
                    for name in names:
                        path=pathlib.Path(parent)/name
                        if path.is_symlink():os.lchown(path,0,0)
                        else:
                            info=path.stat();os.chown(path,0,0);os.chmod(path,0o555 if info.st_mode&0o111 else 0o444)
                require(public_cache.inventory(output,allow_links=True)==inventory,'cache freeze changed contents')
                # Flush the bounded filesystem before making the durable cache
                # reference. Crash before the final record never admits bytes.
                subprocess.run(['/usr/bin/sync','-f',str(work)],check=True)
                subprocess.run(['/usr/bin/mount','-o','remount,ro',str(work)],check=True)
                receipt={'version':1,'cacheKey':plan['cacheKey'],'joined':True,'inventorySha256':inventory['sha256'],'bytes':inventory['bytes'],'entries':inventory['entries']}
                publication={'version':1,'plan':plan,'attemptId':identifier,'receipt':receipt,'image':cache_image_identity(job)}
                private_write(job/'publication.json',encode(publication))
                published=True  # Retain this readonly image even if pointer publication fails.
    except BaseException:
        problem='dependency preparation failed; no partial cache was admitted'
    result={'version':1,'custody':custody,'joined':joined,'plan':plan,'receipt':receipt if problem is None else None,'error':problem}
    private_write(job/'result.json',encode(result))
    if problem is None and publication is not None:
        (ROOT/'cache').mkdir(mode=0o711,exist_ok=True)
        cache=ROOT/'cache'/plan['cacheKey'];cache.mkdir(mode=0o711)
        private_write(cache/'record.json',encode(publication))
    if joined and work is not None and not published and os.path.ismount(work):
        subprocess.run(['/usr/bin/umount',str(work)],check=True)
    return None

def verified_cache(plan,remount=False):
    import public_cache
    cache=ROOT/'cache'/plan['cacheKey']
    record=decode(read_regular(cache/'record.json',4*1024*1024))
    closed(record,('version','plan','attemptId','receipt','image'))
    require(record['version']==1 and record['plan']==plan and IDENT.fullmatch(record['attemptId']),'cache provenance')
    job=ROOT/'preloads'/record['attemptId']
    result=cache_outcome(job)
    require(result['joined'] is True and result['receipt']==record['receipt'] and result['error'] is None,'cache terminal receipt')
    if remount:ensure_cache_mount(job,record)
    return validate_cache_bytes(job,record['receipt'])

def cache_outcome(job):
    path=job/'recovered.json' if (job/'recovered.json').exists() else job/'result.json'
    value=decode(read_regular(path,4*1024*1024))
    closed(value,('version','custody','joined','plan','receipt','error'))
    require(value['version']==1 and value['custody']==decode(read_regular(job/'custody.json',32768)) and type(value['joined']) is bool,'preload outcome')
    return value

def cache_service_absent(job):
    identifier=job.name
    require(IDENT.fullmatch(identifier) and identifier.startswith('cache_'),'preload ID')
    custody=decode(read_regular(job/'custody.json',32768))
    started=decode(read_regular(job/'service-started.json',32768))
    closed(started,('version','custody','control'))
    require(started['version']==1 and started['custody']==custody, 'preload never observed its actual service start; retain custody')
    unit='xcb-preload-'+identifier+'.service'
    expected='/sys/fs/cgroup/system.slice/'+unit+'/worker'
    control=started['control']
    closed(control,('path','dev','ino'))
    require(control['path']==expected and type(control['dev']) is int and control['dev']>0 and type(control['ino']) is int and control['ino']>0,'preload started cgroup identity')
    require(decode(read_regular(job/'cgroup.json',8192))==control,'preload cgroup changed')
    observed=subprocess.run(['/usr/bin/systemctl','show',unit,'--property=MainPID,ControlGroup,ActiveState,SubState,LoadState'],capture_output=True,check=False,timeout=10)
    require(observed.returncode in (0,1),'preload service observation')
    state=dict(line.split('=',1) for line in observed.stdout.decode().splitlines() if '=' in line)
    require(state.get('MainPID')=='0' and state.get('ControlGroup')=='' and state.get('ActiveState') in ('inactive','failed'),'preload service is still active')
    service=pathlib.Path('/sys/fs/cgroup/system.slice')/unit
    require(not service.exists(),'preload service cgroup remains')
    if (job/'child-attempted.json').exists():
        control=decode(read_regular(job/'cgroup.json',8192))
        require(control['path']==str(service/'worker') and type(control['dev']) is int and control['dev']>0 and type(control['ino']) is int and control['ino']>0,'preload cgroup provenance')
        require(decode(read_regular(job/'child-attempted.json',32768))=={'version':1,'custody':decode(read_regular(job/'custody.json',32768))},'preload launch provenance')
    return state

def cache_recover_attempt(job):
    custody=decode(read_regular(job/'custody.json',32768))
    try:result=cache_outcome(job)
    except FileNotFoundError:result=None
    if result is not None and result['joined'] is True:return result
    # The actual service-start witness proves systemd submission completed;
    # a submitter which never reached that witness is never inferred joined.
    # A unique stopped service and absent original cgroup prove all
    # namespace writers are gone. Lost streams are discarded; never publish.
    state=cache_service_absent(job)
    proof={'version':1,'custody':custody,'service':state,'outputDiscarded':True}
    proof_path=job/'recovery-proof.json'
    if proof_path.exists():
        old=decode(read_regular(proof_path,32768));require(old['custody']==custody and old['outputDiscarded'] is True,'recovery evidence changed')
    else:private_write(proof_path,encode(proof))
    plan=decode(read_regular(job/'plan.json',4*1024*1024)) if (job/'plan.json').exists() else None
    value={'version':1,'custody':custody,'joined':True,'plan':plan,'receipt':None,'error':'interrupted dependency preparation joined without publishing partial bytes'}
    path=job/'recovered.json'
    if path.exists():require(decode(read_regular(path,4*1024*1024))==value,'preload recovery changed')
    else:private_write(path,encode(value))
    return value

def cache_status(key,recover=False):
    require(HEX.fullmatch(key),'cache key')
    fd=None
    try:
        if recover:
            fd=os.open(ROOT/'admission.lock',os.O_RDWR|os.O_NOFOLLOW)
            fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
        attempts=[];preload=ROOT/'preloads'
        for job in preload.iterdir() if preload.exists() else []:
            custody=decode(read_regular(job/'custody.json',32768))
            if custody['expectedCacheKey']==key:attempts.append(job)
        require(attempts,'no matching dependency preparation intent')
        receipt=None;joined=True
        for job in attempts:
            try:result=cache_recover_attempt(job) if recover else cache_outcome(job)
            except FileNotFoundError:
                joined=False;continue
            if result['joined'] is not True:joined=False
            elif result['receipt'] is not None and result['error'] is None:
                if recover and not (ROOT/'cache'/key/'record.json').exists():
                    publication=decode(read_regular(job/'publication.json',4*1024*1024))
                    require(publication['attemptId']==job.name and publication['receipt']==result['receipt'] and publication['plan']==result['plan'],'cache publication provenance')
                    ensure_cache_mount(job,publication)
                    validate_cache_bytes(job,result['receipt'])
                    (ROOT/'cache').mkdir(mode=0o711,exist_ok=True)
                    cache=ROOT/'cache'/key;cache.mkdir(mode=0o711,exist_ok=True)
                    private_write(cache/'record.json',encode(publication))
                try:receipt=verified_cache(result['plan'],remount=recover)
                except (FileNotFoundError,ValueError):
                    if recover:raise
                    receipt=None
        return {'version':1,'cacheKey':key,'joined':joined,'prepared':joined and receipt is not None,'receipt':receipt if joined else None}
    finally:
        if fd is not None:os.close(fd)

def cache_image_identity(job):
    image=job/'work.img';info=image.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_uid==0 and info.st_nlink==1 and info.st_mode&0o077==0 and info.st_size==1024*1024*1024,'cache backing image')
    return {'dev':info.st_dev,'ino':info.st_ino,'size':info.st_size}

def ensure_cache_mount(job,record):
    require(cache_image_identity(job)==record['image'],'cache image identity changed')
    work=job/'work'
    if not os.path.ismount(work):
        # Explicit prepare/recover only. No journal replay or package process
        # runs during remount, and the complete content inventory is rehashed.
        cache_service_absent(job)
        info=work.lstat()
        require(stat.S_ISDIR(info.st_mode) and info.st_uid==0 and not any(work.iterdir()),'cache mountpoint changed')
        subprocess.run(['/usr/bin/mount','-o','loop,ro,noload,nodev,nosuid',str(job/'work.img'),str(work)],check=True)
    require(os.statvfs(work).f_flag&os.ST_RDONLY,'cache mount is writable')

def validate_cache_bytes(job,receipt):
    import public_cache
    work=job/'work'
    require(os.path.ismount(work) and os.statvfs(work).f_flag&os.ST_RDONLY,'cache must be mounted readonly')
    inventory=public_cache.inventory(work/'materialize-output',allow_links=True)
    expected={'version':1,'cacheKey':receipt['cacheKey'],'joined':True,'inventorySha256':inventory['sha256'],'bytes':inventory['bytes'],'entries':inventory['entries']}
    require(receipt==expected,'cached bytes changed')
    return expected

def cache_ack(ack):
    require(HEX.fullmatch(ack['cacheKey']),'cache acknowledgment key')
    fd=os.open(ROOT/'admission.lock',os.O_RDWR|os.O_NOFOLLOW)
    try:
        fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
        preload=ROOT/'preloads';matched=False
        for job in preload.iterdir() if preload.exists() else []:
            try:result=cache_outcome(job)
            except FileNotFoundError:continue
            key=result['plan']['cacheKey'] if result['plan'] else result['custody']['expectedCacheKey']
            if key!=ack['cacheKey']:continue
            require(result['joined'] is True,'cannot acknowledge pending preload')
            cache_service_absent(job)
            matched=True
            cache=ROOT/'cache'/ack['cacheKey']/'record.json'
            if cache.exists() and decode(read_regular(cache,4*1024*1024))['attemptId']==job.name:
                verified_cache(result['plan'])
                continue  # Published immutable dependency bytes remain available.
            evidence=job/'ack.json'
            if not evidence.exists():private_write(evidence,encode(ack))
            else:require(decode(read_regular(evidence,16384))['cacheKey']==ack['cacheKey'],'preload acknowledgment changed')
            work=job/'work'
            if os.path.ismount(work):subprocess.run(['/usr/bin/umount',str(work)],check=True)
            image=job/'work.img'
            if image.exists():
                info=image.lstat()
                require(stat.S_ISREG(info.st_mode) and info.st_uid==0 and info.st_nlink==1 and info.st_size==1024*1024*1024,'preload scratch image')
                image.unlink()
            if work.exists():
                require(work.is_dir() and not work.is_symlink(),'preload scratch directory')
                work.rmdir()
            directory=os.open(job,os.O_RDONLY|os.O_DIRECTORY)
            try:os.fsync(directory)
            finally:os.close(directory)
        require(matched,'no joined preload matches acknowledgment')
        return {'acknowledged':True}
    finally:os.close(fd)


def main():
    require(os.geteuid() == 0, 'root supervisor required')
    operation = sys.argv[1]
    if operation in ('public-cache-plan','public-cache-prepare'):
        value = cache_operation(operation)
    elif operation == 'public-cache-supervise':
        value = cache_supervise(sys.argv[2])
    elif operation in ('public-cache-status','public-cache-recover','public-cache-ack'):
        request=decode(sys.stdin.buffer.read(16385))
        if operation=='public-cache-ack':
            closed(request,('version','cacheKey','hostReceiptSha256'))
            require(request['version']==1 and HEX.fullmatch(request['hostReceiptSha256']),'cache acknowledgment')
            value=cache_ack(request)
        else:
            closed(request,('version','cacheKey'))
            require(request['version']==1,'cache status version')
            value=cache_status(request['cacheKey'],operation=='public-cache-recover')
    elif operation == 'inspect':
        value = inspect()
    elif operation == 'run':
        value = receive()
    elif operation == 'supervise':
        value = supervise(sys.argv[2])
    elif operation == 'status':
        value = status(sys.argv[2])
    elif operation == 'ack':
        value = acknowledge(sys.argv[2])
    elif operation == 'recover-prelaunch':
        request=decode(sys.stdin.buffer.read(16385))
        closed(request,('custody','guestSha256'))
        require(request['custody']['commandId']==sys.argv[2] and HEX.fullmatch(request['guestSha256']), 'prelaunch request binding')
        fd=os.open(ROOT/'admission.lock',os.O_RDWR|os.O_NOFOLLOW)
        try:
            fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
            value=reconcile_prelaunch(request['custody'],request['guestSha256'])
        finally:os.close(fd)
    elif operation == 'cancel':
        identifier = sys.argv[2]
        job_path(identifier)
        try:
            private_write(ROOT / 'cancel' / identifier, b'cancel\n')
        except FileExistsError:
            pass
        value = {'cancelled': True}
    else:
        raise ValueError('unknown operation')
    if value is not None:
        sys.stdout.buffer.write(encode(value) + b'\n')

if __name__ == '__main__':
    try:
        main()
    except BaseException as error:
        print('xcb-command: ' + type(error).__name__, file=sys.stderr)
        sys.exit(1)
