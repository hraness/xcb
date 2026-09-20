#!/usr/bin/env python3
"""Prepare public locked dependencies in the separately qualified XCB guest.

The default is a no-download plan. No host package manager, Git, workspace code,
configuration or credential is executed/read. Run through hra-host-run on macOS.
"""
import argparse
import base64
import contextlib
import fcntl
import hashlib
import types
import json
import os
from pathlib import Path
import re
import select
import selectors
import signal
import stat
import subprocess
import sys
import tempfile
import time

LIMIT = 1024 ** 3
MAX_FILE = 2 * 1024 ** 2
MAX_INPUT = 8 * 1024 ** 2
MAX_OUTPUT = 4 * 1024 ** 2
TOOLS = ('bun', 'cargo', 'git', 'node', 'python', 'rustc')
SHA = re.compile(r'^[0-9a-f]{64}$')
EXCLUDED = {'.git', 'node_modules', 'target', 'dist', 'build', '.next', '.venv',
            'venv', '__pycache__', '.cache', '.ssh', '.aws', '.gnupg', '.codex',
            '.claude', '.devin', '.npmrc', '.pypirc', '.netrc', '.DS_Store'}
REQUIRED_CASES = ('file-edit-and-python', 'uid-filesystem-network-and-userns',
                  'detached-setsid-closed-stdio', 'deadline-kills-descendants',
                  'output-overflow-joins', 'pre-cancel-never-executes',
                  'offline-language-toolchains', 'peer-work-and-control-denied',
                  'git-projection-unit-semantics', 'readonly-filtered-git-inspection',
                  'public-cache-isolation-and-key-binding', 'offline-cargo-bun-cache-usage')


class Refused(ValueError):
    """Only fixed, host-authored explanations may be printed."""


def require(value, message):
    if not value:
        raise Refused(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def encode(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()


def decode(data):
    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result, 'duplicate JSON field')
            result[key] = value
        return result
    return json.loads(data, object_pairs_hook=pairs,
                      parse_constant=lambda _: require(False, 'nonfinite JSON'))


def stamp(s):
    return (s.st_dev, s.st_ino, s.st_mode, s.st_uid, s.st_nlink, s.st_size,
            s.st_mtime_ns, s.st_ctime_ns)


def physical(path):
    require(path.is_absolute() and path.resolve(strict=True) == path, 'select an absolute physical path')
    return path


def excluded(name):
    return (name in EXCLUDED or name == '.env' or name.startswith('.xcb-') or
            (name.startswith('.env.') and name not in ('.env.example', '.env.sample', '.env.template')))


class Capture:
    """Retain descriptor and pathname identity, then recheck every captured input."""
    def __init__(self):
        self.rows = []
        self.mutable_directories = set()

    def directory(self, path, private=False, parent=None, mutable=False):
        fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                     dir_fd=parent)
        try:
            s = os.fstat(fd)
            require(s.st_uid == os.getuid(), 'foreign directory owner')
            require(not private or s.st_mode & 0o077 == 0, 'command root must be private')
            require(stamp(s) == stamp(os.stat(path, dir_fd=parent, follow_symlinks=False)),
                    'directory changed while opening')
            self.rows.append((fd, path, parent, stamp(s), None))
            if mutable:
                self.mutable_directories.add(fd)
            return fd
        except BaseException:
            os.close(fd)
            raise

    def file(self, path, maximum, private=False, parent=None, owner=True):
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC,
                     dir_fd=parent)
        try:
            s = os.fstat(fd)
            require(stat.S_ISREG(s.st_mode) and s.st_nlink == 1 and s.st_size <= maximum,
                    'input must be a bounded regular single-link file')
            require(not owner or s.st_uid == os.getuid(), 'foreign file owner')
            require(not private or s.st_mode & 0o077 == 0, 'command state must be private')
            chunks, size = [], 0
            while True:
                chunk = os.read(fd, min(65536, maximum + 1 - size))
                if not chunk:
                    break
                chunks.append(chunk)
                size += len(chunk)
                require(size <= maximum, 'input exceeds byte limit')
            data = b''.join(chunks)
            require(stamp(s) == stamp(os.fstat(fd)) == stamp(os.stat(path, dir_fd=parent, follow_symlinks=False)),
                    'input changed while reading')
            self.rows.append((fd, path, parent, stamp(s), sha(data)))
            return data
        except BaseException:
            os.close(fd)
            raise

    def verify(self):
        for fd, path, parent, before, digest in self.rows:
            current, named = stamp(os.fstat(fd)), stamp(os.stat(path, dir_fd=parent, follow_symlinks=False))
            stable = current[:4] == before[:4] == named[:4] if fd in self.mutable_directories else current == before == named
            require(stable, 'inputs changed; rerun dependency preparation')
            if digest is not None:
                os.lseek(fd, 0, os.SEEK_SET)
                hasher = hashlib.sha256()
                remaining = before[5]
                while remaining:
                    chunk = os.read(fd, min(65536, remaining))
                    require(chunk, 'input shortened')
                    hasher.update(chunk)
                    remaining -= len(chunk)
                require(not os.read(fd, 1) and hasher.hexdigest() == digest and stamp(os.fstat(fd)) == before,
                        'input bytes changed; rerun dependency preparation')

    def close(self):
        for row in reversed(self.rows):
            os.close(row[0])
        self.rows.clear()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def manifests(capture, workspace):
    root = capture.directory(physical(workspace))
    root_names = os.listdir(root)
    cargo = 'Cargo.lock' in root_names
    bun = 'bun.lock' in root_names
    require(cargo or bun, 'workspace needs a root Cargo.lock or bun.lock')
    require(not cargo or 'Cargo.toml' in root_names, 'Cargo.lock requires root Cargo.toml')
    require(not bun or 'package.json' in root_names, 'bun.lock requires root package.json')
    names = ({'Cargo.toml'} if cargo else set()) | ({'package.json'} if bun else set())
    files, nested_locks, visited, total = [], [], 0, 0

    def walk(fd, prefix, depth):
        nonlocal visited, total
        require(depth <= 64, 'workspace depth limit')
        entries = os.listdir(fd)
        visited += len(entries)
        require(visited <= 8192, 'workspace metadata entry limit')
        for name in sorted(entries):
            require(not any(ord(c) < 32 or ord(c) == 127 for c in name), 'workspace filename controls')
            path = prefix + name
            require(len(path.encode('utf-8')) <= 4096, 'workspace path limit')
            if excluded(name):
                continue
            s = os.stat(name, dir_fd=fd, follow_symlinks=False)
            if stat.S_ISDIR(s.st_mode):
                child = capture.directory(name, parent=fd)
                walk(child, path + '/', depth + 1)
            elif name in names or (not prefix and name in ('Cargo.lock', 'bun.lock')):
                require(len(files) < 256, 'manifest count limit')
                data = capture.file(name, MAX_FILE, parent=fd)
                total += len(data)
                require(total <= MAX_INPUT, 'manifest aggregate byte limit')
                files.append({'path': path, 'base64': base64.b64encode(data).decode(), 'sha256': sha(data)})
            elif prefix and name in ('Cargo.lock', 'bun.lock'):
                require(len(nested_locks) < 256, 'nested lockfile count limit')
                nested_locks.append(path)
            elif not stat.S_ISREG(s.st_mode):
                raise Refused('workspace refuses symlinks and special files')
    walk(root, '', 0)
    capture.verify()
    return sorted(files, key=lambda row: row['path']), nested_locks


def private_root(capture, root):
    rootfd = capture.directory(physical(root), private=True, mutable=True)
    require(capture.file('xcb-owner.json', 128, True, rootfd) == b'{"owner":"xcb-command-v1"}\n',
            'not an XCB-owned command root')
    for name in ('home', 'lima', 'cache', 'jobs'):
        # Validate these roots but not mutable VM internals or unrelated jobs.
        fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=rootfd)
        try:
            s = os.fstat(fd)
            require(s.st_uid == os.getuid() and s.st_mode & 0o077 == 0, 'unsafe private backend directory')
        finally:
            os.close(fd)
    return rootfd


@contextlib.contextmanager
def admission(rootfd):
    fd = os.open('admission.lock', os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=rootfd)
    try:
        s = os.fstat(fd)
        require(stat.S_ISREG(s.st_mode) and s.st_nlink == 1 and s.st_uid == os.getuid()
                and s.st_mode & 0o077 == 0 and s.st_size <= 64, 'unsafe backend admission lock')
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        require(stamp(s) == stamp(os.stat('admission.lock', dir_fd=rootfd, follow_symlinks=False)),
                'backend admission lock changed')
        yield
        require(stamp(s) == stamp(os.fstat(fd)) == stamp(os.stat('admission.lock', dir_fd=rootfd, follow_symlinks=False)),
                'backend admission lock changed')
    finally:
        os.close(fd)


def backend(capture, rootfd, source):
    value = decode(capture.file('backend.json', 65536, True, rootfd))
    require(value.get('version') == 1 and value.get('bounds', {}).get('cacheBytes') == LIMIT,
            'command backend needs qualified public-cache support; run explicit setup refresh')
    directory = source / 'crates/xcb-runtime/src/command'
    for field, name in (('guestSha256', 'guest.py'), ('publicCacheSha256', 'public_cache.py'),
                        ('policySha256', 'lima.yaml')):
        require(value.get(field) == sha(capture.file(directory / name, 2 * MAX_FILE)),
                'command source changed; explicit setup refresh and qualification required')
    tools = value.get('toolDigests', {})
    require(all(isinstance(tools.get(name), dict) and SHA.fullmatch(tools[name].get('sha256', ''))
                and tools[name].get('path', '').startswith('/') for name in TOOLS), 'backend tool identity')
    executable = physical(Path(value['limaExecutable']))
    require(executable.stat().st_mode & 0o111 and sha(capture.file(executable, 256 * 1024 ** 2, owner=False))
            == value['limaSha256'], 'Lima executable changed')
    qualification = value.get('qualification', {})
    require(set(qualification) == {'suiteSha256', 'environmentSha256', 'evidenceSha256'}
            and all(isinstance(v, str) and SHA.fullmatch(v) for v in qualification.values()),
            'backend qualification binding')
    environment = {key: row for key, row in value.items() if key != 'qualification'}
    require(sha(encode(environment)) == qualification['environmentSha256']
            and sha(capture.file(directory / 'test_live.py', MAX_FILE)) == qualification['suiteSha256'],
            'backend qualification source changed')
    raw = capture.file('qualification-' + qualification['evidenceSha256'] + '.json', MAX_FILE, True, rootfd)
    evidence = decode(raw)
    require(sha(raw) == qualification['evidenceSha256'] and evidence.get('version') == 1
            and evidence.get('environmentSha256') == qualification['environmentSha256']
            and evidence.get('suiteSha256') == qualification['suiteSha256']
            and isinstance(evidence.get('cases'), list)
            and tuple(row.get('name') for row in evidence['cases']) == REQUIRED_CASES,
            'complete backend qualification required')
    for row in evidence['cases']:
        response = row.get('response')
        require(isinstance(response, str) and sha(response.encode()) == row.get('resultSha256'),
                'backend qualification response changed')
        result = decode(response)
        require(result.get('version') == 1 and result.get('joined') is True and result.get('error') is None
                and result.get('custody', {}).get('backendSha256') == qualification['environmentSha256'],
                'backend qualification did not join')
    return value


def transport(root, value, operation, data, timeout):
    require(operation in ('public-cache-plan', 'public-cache-prepare', 'public-cache-status', 'public-cache-recover', 'public-cache-ack'), 'unknown cache operation')
    argv = [value['limaExecutable'], 'shell', '--workdir', '/', 'worker', 'sudo', '-n',
            '/usr/bin/python3', '/usr/local/lib/xcb-command/guest.py', operation]
    env = {'HOME': str(root / 'home'), 'LIMA_HOME': str(root / 'lima'),
           'XDG_CACHE_HOME': str(root / 'cache'), 'SSH': '/usr/bin/ssh',
           'PATH': '/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin', 'LANG': 'en_US.UTF-8'}
    return capture_client(argv, env, data, timeout)


def capture_client(argv, env, data, timeout):
    """Join local transport independently. Lost remote joins never become success."""
    require(len(data) <= 16 * 1024 ** 2, 'cache request byte limit')
    output = bytearray()
    with tempfile.TemporaryFile() as request:
        request.write(data)
        request.seek(0)
        previous = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
        process = None
        watcher = select.kqueue() if hasattr(selectors.select, 'kqueue') else None
        reaped = False
        pipes_joined = False
        try:
            process = subprocess.Popen(argv, stdin=request, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       env=env, start_new_session=True, close_fds=True,
                                       preexec_fn=lambda: signal.pthread_sigmask(signal.SIG_SETMASK, previous))
            if watcher is not None:
                event = select.kevent(process.pid, filter=select.KQ_FILTER_PROC,
                    flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                    fflags=select.KQ_NOTE_EXIT)
                try:
                    watcher.control([event], 0, 0)
                except ProcessLookupError:
                    # A child that exited before registration remains unreaped.
                    # The mandatory pre-reap stop/reap below handles this race.
                    watcher.close()
                    watcher = None
                    observed_exit = True
                else:
                    observed_exit = False
            else:
                observed_exit = False
            signal.pthread_sigmask(signal.SIG_SETMASK, previous)
            with selectors.DefaultSelector() as selector:
                for pipe in (process.stdout, process.stderr):
                    os.set_blocking(pipe.fileno(), False)
                    selector.register(pipe, selectors.EVENT_READ)
                deadline, captured = time.monotonic() + timeout, 0
                while selector.get_map():
                    require(time.monotonic() < deadline, 'guest response deadline; preserve backend custody and inspect before retry')
                    for key, _ in selector.select(min(0.2, max(0, deadline - time.monotonic()))):
                        chunk = os.read(key.fileobj.fileno(), 65536)
                        if not chunk:
                            selector.unregister(key.fileobj)
                        else:
                            captured += len(chunk)
                            require(captured <= MAX_OUTPUT, 'guest response overflow; preserve backend custody')
                            if key.fileobj is process.stdout:
                                output.extend(chunk)
                pipes_joined = True
                while not observed_exit:
                    require(time.monotonic() < deadline, 'guest transport did not exit; preserve backend custody')
                    if watcher is not None:
                        observed_exit = bool(watcher.control([], 1, min(0.2, deadline - time.monotonic())))
                    else:
                        observed_exit = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None
                        if not observed_exit:
                            time.sleep(0.02)
            # Leader is still unreaped, so group identity cannot be reused.
        finally:
            signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
            try:
                if process is not None:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except (ProcessLookupError, PermissionError):
                        # Darwin may report EPERM for an unreaped zombie group.
                        # This is not stop proof: bounded wait and fresh absence
                        # below are both still mandatory. Never signal after wait.
                        pass
                    try:
                        process.wait(timeout=10)
                        reaped = True
                    finally:
                        for pipe in (process.stdout, process.stderr):
                            pipe.close()
                    try:
                        os.killpg(process.pid, 0)
                    except ProcessLookupError:
                        pass
                    else:
                        raise Refused('local transport group join unproven; preserve backend custody')
            finally:
                if watcher is not None:
                    watcher.close()
                signal.pthread_sigmask(signal.SIG_SETMASK, previous)
    require(reaped and pipes_joined and process.returncode == 0,
            'guest rejected dependency preparation; no joined cache receipt; inspect backend before retry')
    return bytes(output)


def validate_plan(value, spec, source, expected_hash):
    # Import only the trusted, already hash-bound recipe; no consumer module.
    path = source / 'crates/xcb-runtime/src/command/public_cache.py'
    with Capture() as inputs:
        source_bytes = inputs.file(path, 2 * MAX_FILE)
        require(sha(source_bytes) == expected_hash, 'preloader source changed')
        module = types.ModuleType('xcb_public_cache')
        module.__file__ = str(path)
        exec(compile(source_bytes, str(path), 'exec'), module.__dict__)
        inputs.verify()
    module.validate_plan(value)
    expected = [{'path': row['path'], 'sha256': row['sha256']} for row in spec['files']]
    require(value['inputs'] == expected and value['toolDigests'] == spec['toolDigests']
            and value['preloaderSha256'] == expected_hash, 'guest planned different inputs')
    return value


def validate_receipt(value, key):
    require(isinstance(value, dict) and set(value) == {'version', 'cacheKey', 'joined', 'inventorySha256', 'bytes', 'entries'}
            and value['version'] == 1 and value['cacheKey'] == key and value['joined'] is True
            and isinstance(value['inventorySha256'], str) and SHA.fullmatch(value['inventorySha256'])
            and type(value['bytes']) is int and 0 <= value['bytes'] <= LIMIT
            and type(value['entries']) is int and 0 <= value['entries'] <= 200000,
            'guest cache receipt is incomplete or does not match the plan')
    return value


def durable_create(rootfd, name, value, maximum=65536):
    """No-clobber state publication; a partial file is retained and blocks retry."""
    data = encode(value)
    require(len(data) <= maximum, 'preparation record limit')
    previous = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
    try:
        fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                     0o600, dir_fd=rootfd)
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.fsync(rootfd)
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous)



def record_plan(rootfd, value, plan):
    record = {'version': 1, 'operation': 'public-cache-plan',
              'backendSha256': sha(encode(value)), 'plan': plan}
    digest = sha(encode(record))
    name = 'public-cache-plan-' + plan['cacheKey'] + '-' + digest + '.json'
    try:
        durable_create(rootfd, name, record, MAX_OUTPUT)
    except FileExistsError:
        with Capture() as capture:
            require(decode(capture.file(name, MAX_OUTPUT, True, rootfd)) == record,
                    'stored plan evidence changed')
    return digest


def acknowledge(root, value, key, receipt_sha, terminal=False):
    try:
        result = decode(transport(root, value, 'public-cache-ack',
            encode({'version': 1, 'cacheKey': key, 'hostReceiptSha256': receipt_sha}), 30))
        return not (type(result) is dict and result == {'acknowledged': True}
                    and type(result['acknowledged']) is bool)
    except InterruptedError:
        if not terminal:
            raise  # Cancellation during plan cleanup must never start a fetch.
        return True
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError):
        # Exact joined terminal evidence is already durable. Scratch cleanup
        # failure cannot rewrite it into an unjoined or unprepared outcome.
        return True


def begin_intent(rootfd, key, backend_value, spec):
    entries = os.listdir(rootfd)
    require(len(entries) <= 4096, 'command root entry limit')
    require(not any(name.startswith('public-cache-intent-') for name in entries),
            'a preparation intent is retained; use --status or --recover with its --cache-key before preparing again')
    require(isinstance(key, str) and SHA.fullmatch(key), 'invalid cache key')
    value = {'version': 1, 'operation': 'public-cache-prepare', 'cacheKey': key,
             'backendSha256': sha(encode(backend_value)), 'specSha256': sha(encode(spec))}
    try:
        durable_create(rootfd, 'public-cache-intent-' + key + '.json', value)
    except FileExistsError:
        raise Refused('preparation intent already exists; use --status or --recover with --cache-key before preparing again') from None
    return value


def read_intent(capture, rootfd, key):
    require(isinstance(key, str) and SHA.fullmatch(key), 'invalid cache key')
    value = decode(capture.file('public-cache-intent-' + key + '.json', 65536, True, rootfd))
    require(isinstance(value, dict) and set(value) == {'version', 'operation', 'cacheKey', 'backendSha256', 'specSha256'}
            and value['version'] == 1 and value['operation'] == 'public-cache-prepare' and value['cacheKey'] == key
            and all(isinstance(value[field], str) and SHA.fullmatch(value[field])
                    for field in ('backendSha256', 'specSha256')), 'preparation intent mismatch; preserve state')
    return value


def validate_terminal(value, key):
    require(isinstance(value, dict) and set(value) == {'version', 'cacheKey', 'joined', 'prepared', 'receipt'}
            and value['version'] == 1 and value['cacheKey'] == key
            and type(value['joined']) is bool and type(value['prepared']) is bool,
            'guest terminal preparation evidence mismatch')
    if value['prepared']:
        require(value['joined'], 'prepared cache lacks a joined proof')
        validate_receipt(value['receipt'], key)
    else:
        require(value['receipt'] is None, 'unexpected preparation receipt')
    return value


def finish_intent(rootfd, intent, terminal):
    validate_terminal(terminal, intent['cacheKey'])
    require(terminal['joined'], 'preparation remains unjoined; preserve intent and backend custody')
    with Capture() as capture:
        require(read_intent(capture, rootfd, intent['cacheKey']) == intent, 'preparation intent changed')
        result = {'version': 1, 'intent': intent, 'terminal': terminal}
        name = 'public-cache-result-' + intent['cacheKey'] + '-' + sha(encode(result)) + '.json'
        try:
            durable_create(rootfd, name, result)
        except FileExistsError:
            require(decode(capture.file(name, 65536, True, rootfd)) == result, 'preparation terminal record changed')
        capture.verify()
        previous = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
        try:
            capture.verify()
            os.unlink('public-cache-intent-' + intent['cacheKey'] + '.json', dir_fd=rootfd)
            os.fsync(rootfd)
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, previous)
        return sha(encode(result))


def reconcile(root, key, recover, source):
    with Capture() as capture:
        rootfd = private_root(capture, root)
        with admission(rootfd):
            value = backend(capture, rootfd, source)
            with Capture() as intent_capture:
                intent = read_intent(intent_capture, rootfd, key)
                require(intent['backendSha256'] == sha(encode(value)), 'preparation backend changed; retain original custody')
                operation = 'public-cache-recover' if recover else 'public-cache-status'
                result = validate_terminal(decode(transport(root, value, operation,
                    encode({'version': 1, 'cacheKey': key}), 90)), key)
                capture.verify()
                intent_capture.verify()
                cleanup_pending = False
                if recover and result['joined']:
                    receipt_sha = finish_intent(rootfd, intent, result)
                    cleanup_pending = acknowledge(root, value, key, receipt_sha, terminal=True)
                return {'version': 1, 'operation': operation, 'cacheKey': key,
                        'intentRetained': not (recover and result['joined']), 'terminal': result,
                        'cleanupPending': cleanup_pending}


def run(root, workspace, prepare, source):
    with Capture() as capture:
        rootfd = private_root(capture, root)
        with admission(rootfd):
            value = backend(capture, rootfd, source)
            files, nested = manifests(capture, workspace)
            spec = {'version': 1, 'toolDigests': {name: value['toolDigests'][name]['sha256'] for name in TOOLS}, 'files': files}
            payload = encode(spec)
            capture.verify()
            plan = validate_plan(decode(transport(root, value, 'public-cache-plan', payload, 60)),
                                 spec, source, value['publicCacheSha256'])
            capture.verify()
            plan_receipt = record_plan(rootfd, value, plan)
            cleanup_pending = acknowledge(root, value, plan['cacheKey'], plan_receipt)
            capture.verify()
            result = {'version': 1, 'operation': 'prepare' if prepare else 'plan',
                      'cacheKey': plan['cacheKey'], 'inputs': plan['inputs'],
                      'nestedLockfilesNotIncluded': nested,
                      'archives': len(plan['archives']), 'repositories': len(plan['repositories']),
                      'prepared': False, 'cleanupPending': cleanup_pending}
            if prepare:
                intent = begin_intent(rootfd, plan['cacheKey'], value, spec)
                request = encode({'version': 1, 'expectedCacheKey': plan['cacheKey'], 'spec': spec})
                receipt = validate_receipt(decode(transport(root, value, 'public-cache-prepare', request, 1300)), plan['cacheKey'])
                capture.verify()
                receipt_sha = finish_intent(rootfd, intent, {'version': 1, 'cacheKey': plan['cacheKey'],
                              'joined': True, 'prepared': True, 'receipt': receipt})
                cleanup_pending = acknowledge(root, value, plan['cacheKey'], receipt_sha, terminal=True)
                result.update(prepared=True, receipt=receipt, cleanupPending=cleanup_pending)
            return result


def interrupted(_signal, _frame):
    raise InterruptedError('dependency preparation interrupted; retain custody')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True, help='existing qualified, private XCB command root')
    parser.add_argument('--workspace', type=Path, help='absolute physical project directory with root lockfiles')
    action = parser.add_mutually_exclusive_group()
    action.add_argument('--dry-run', action='store_true', help='plan only, without downloads (default)')
    action.add_argument('--prepare', action='store_true', help='prepare the reviewed public dependencies inside the guest')
    action.add_argument('--status', action='store_true', help='read retained preparation status; never resubmit or clear intent')
    action.add_argument('--recover', action='store_true', help='join/reconcile retained preparation without resubmitting')
    parser.add_argument('--cache-key', help='exact key of a retained preparation, for --status or --recover')
    args = parser.parse_args()
    signal.signal(signal.SIGINT, interrupted)
    signal.signal(signal.SIGTERM, interrupted)
    require(sys.platform == 'darwin', 'this frontend requires the qualified macOS Lima backend')
    require(sys.version_info >= (3, 9) and (hasattr(select, 'kqueue') or hasattr(os, 'waitid')), 'Python3.9+ with unreaped process-exit observation is required')
    source = Path(__file__).resolve().parents[1]
    if args.status or args.recover:
        require(args.cache_key is not None and args.workspace is None, '--status/--recover require --cache-key and no --workspace')
        result = reconcile(args.root, args.cache_key, args.recover, source)
    else:
        require(args.workspace is not None and args.cache_key is None, 'planning/preparing requires --workspace and no --cache-key')
        result = run(args.root, args.workspace, args.prepare, source)
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except Refused as error:
        print('Dependency preparation refused: ' + str(error), file=sys.stderr)
        sys.exit(1)
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError):
        # Do not emit provider or package-manager output, manifest bodies, paths
        # from untrusted parser exceptions, or inherited environment values.
        print('Dependency preparation refused or incomplete. Check root qualification, physical input paths, public lockfiles, and backend custody before retrying.', file=sys.stderr)
        sys.exit(1)
