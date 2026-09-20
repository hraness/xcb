#!/usr/bin/python3
"""Trusted, unprivileged Git projection subprocess; never run a Git parser as root.

The supervisor supplies GitSnapshot JSON on stdin to sandbox_argv(), joins its
cgroup and pipes, then validates the output with validate_output() and mounts
ONLY output/.git read-only into the command worker. Raw Git stays in this
subprocess's private tmpfs. This is a synthetic HEAD/index, not source history.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import selectors
import signal
import stat
import struct
import subprocess
import sys
import tempfile
import time

MAX_RAW = 32 * 1024 * 1024
MAX_INPUT = 64 * 1024 * 1024
MAX_FILE = 2 * 1024 * 1024
MAX_ENTRIES = 4096
MAX_OUTPUT = 64 * 1024 * 1024
MAX_GIT_OUTPUT = 20 * 1024 * 1024
UID = 61001
MODULE = '/usr/local/lib/xcb-command/git_projection.py'
OID = re.compile(r'^[0-9a-f]{40}$')
SHA = re.compile(r'^[0-9a-f]{64}$')
BRANCH = 'refs/heads/xcb-snapshot'
HEAD = ('ref: ' + BRANCH + '\n').encode()
MESSAGE = b'XCB filtered HEAD snapshot; no source history.\n'
BLOCKED = frozenset(('.git', 'node_modules', 'target', 'dist', 'build', '.next', '.venv', 'venv', '__pycache__', '.cache', '.ssh', '.aws', '.gnupg', '.codex', '.claude', '.devin', '.npmrc', '.pypirc', '.netrc', '.DS_Store'))
ENV = {'PATH': '/usr/bin:/bin', 'HOME': '/tmp/empty-home', 'LANG': 'C.UTF-8',
       'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null',
       'GIT_TERMINAL_PROMPT': '0', 'GIT_OPTIONAL_LOCKS': '0', 'GIT_NO_REPLACE_OBJECTS': '1',
       'GIT_ATTR_NOSYSTEM': '1', 'GIT_LFS_SKIP_SMUDGE': '1',
       'GIT_AUTHOR_NAME': 'XCB Snapshot', 'GIT_AUTHOR_EMAIL': 'snapshot@xcb.invalid',
       'GIT_COMMITTER_NAME': 'XCB Snapshot', 'GIT_COMMITTER_EMAIL': 'snapshot@xcb.invalid',
       'GIT_AUTHOR_DATE': '@0 +0000', 'GIT_COMMITTER_DATE': '@0 +0000'}


def require(value, message):
    if not value:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()


def decode(raw):
    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result, 'duplicate JSON field')
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=pairs, parse_constant=lambda _: require(False, 'nonfinite JSON'))


def closed(value, fields):
    require(isinstance(value, dict) and set(value) == set(fields), 'unknown or missing fields')


def relative(path):
    require(isinstance(path, str) and 0 < len(path.encode()) <= 4096, 'path length')
    parts = path.split('/')
    require(len(parts) <= 64 and all(part and part not in ('.', '..') for part in parts), 'relative path')
    require(not any(ord(c) < 32 or ord(c) == 127 for c in path), 'path controls')
    return path


def excluded(path):
    return any(part in BLOCKED or part == '.env'
               or (part.startswith('.env.') and part not in ('.env.example', '.env.sample', '.env.template'))
               or part.startswith('.xcb-') for part in path.split('/'))


def raw_path(path):
    relative(path)
    return (path in ('HEAD', 'index', 'packed-refs')
            or (path.startswith('refs/heads/') and re.fullmatch(r'refs/heads/[A-Za-z0-9_-][A-Za-z0-9._/-]*', path)
                and all(not part.startswith('.') and not part.endswith(('.', '.lock')) and '..' not in part
                        for part in path.split('/')))
            or re.fullmatch(r'objects/[0-9a-f]{2}/[0-9a-f]{38}', path)
            or re.fullmatch(r'objects/pack/pack-[0-9a-f]{40}\.(pack|idx)', path))


def validate_index(raw):
    """Validate v2/v3 index framing/checksum, rejecting all ambiguous modes.

    Required lowercase extensions, extended/assume-valid flags and non-stage0
    entries are unsupported. No original index bytes enter the output.
    """
    require(32 <= len(raw) <= MAX_FILE and raw[:4] == b'DIRC', 'index format')
    version, count = struct.unpack('>II', raw[4:12])
    require(version in (2, 3) and count <= MAX_ENTRIES, 'index version/count')
    require(hashlib.sha1(raw[:-20]).digest() == raw[-20:], 'index checksum')
    offset, seen, entries = 12, set(), []
    for _ in range(count):
        start = offset
        require(offset + 63 <= len(raw) - 20, 'truncated index entry')
        mode = struct.unpack('>I', raw[offset + 24:offset + 28])[0]
        oid = raw[offset + 40:offset + 60].hex()
        flags = struct.unpack('>H', raw[offset + 60:offset + 62])[0]
        require(mode in (0o100644, 0o100755) and flags & 0xf000 == 0, 'unsupported index mode/stage/flags')
        end = raw.find(b'\0', offset + 62, len(raw) - 20)
        require(end >= 0, 'index path terminator')
        name = relative(raw[offset + 62:end].decode('utf-8'))
        length = len(name.encode())
        require((flags & 0xfff) == min(length, 0xfff) and name not in seen, 'index path length/duplicate')
        require(oid != '0' * 40, 'intent-to-add index unsupported')
        require(not entries or entries[-1][2].encode() < name.encode(), 'index path ordering')
        require(all('/'.join(name.split('/')[:i]) not in seen for i in range(1, len(name.split('/')))), 'index file ancestor')
        seen.add(name)
        entries.append((str(oct(mode)[2:]), oid, name))
        offset = start + ((end + 1 - start + 7) // 8) * 8
        require(offset <= len(raw) - 20 and not any(raw[end:offset]), 'index padding')
    while offset < len(raw) - 20:
        require(offset + 8 <= len(raw) - 20, 'truncated index extension')
        signature = raw[offset:offset + 4]
        size = struct.unpack('>I', raw[offset + 4:offset + 8])[0]
        require(65 <= signature[0] <= 90 and signature != b'FSMN', 'required/split/sparse/fsmonitor index unsupported')
        offset += 8 + size
        require(offset <= len(raw) - 20, 'index extension size')
    require(offset == len(raw) - 20, 'index trailing bytes')
    return entries


def validate_snapshot(snapshot):
    closed(snapshot, ('version', 'headObjectId', 'files'))
    require(type(snapshot['version']) is int and snapshot['version'] == 1, 'Git snapshot version')
    head = snapshot['headObjectId']
    require(head is None or isinstance(head, str) and OID.fullmatch(head), 'HEAD object ID')
    require(isinstance(snapshot['files'], list) and 1 <= len(snapshot['files']) <= MAX_ENTRIES, 'raw file count')
    files, total = {}, 0
    for entry in snapshot['files']:
        closed(entry, ('path', 'base64', 'sha256'))
        name = entry['path']
        require(raw_path(name) and name not in files, 'raw Git path')
        require(isinstance(entry['base64'], str) and len(entry['base64']) <= ((MAX_FILE + 2) // 3) * 4, 'raw encoding size')
        data = base64.b64decode(entry['base64'], validate=True)
        require(len(data) <= MAX_FILE and isinstance(entry['sha256'], str) and SHA.fullmatch(entry['sha256'])
                and sha(data) == entry['sha256'], 'raw file digest/size')
        total += len(data)
        require(total <= MAX_RAW, 'raw Git aggregate limit')
        files[name] = data
    require('HEAD' in files, 'HEAD missing')
    for name in files:
        parts = name.split('/')
        require(all('/'.join(parts[:i]) not in files for i in range(1, len(parts))), 'raw file ancestor')
    head_text = files['HEAD'].decode('ascii').rstrip('\n')
    require(head_text == head or head_text.startswith('ref: refs/heads/') and raw_path(head_text[5:]), 'HEAD shape')
    index = validate_index(files['index']) if 'index' in files else []
    return head, files, index


def output_path(path):
    return path in ('HEAD', 'index', BRANCH) or re.fullmatch(r'objects/[0-9a-f]{2}/[0-9a-f]{38}', path)


def validate_output(output):
    """Root-safe validation after independent join: filesystem/hash only, no Git.

    Returns a digest of exact file names/bytes/modes for comparison with stdout.
    Caller must freeze this directory and use a readonly mount for the worker.
    """
    root = Path(output) / '.git'
    info = root.lstat()
    require(stat.S_ISDIR(info.st_mode) and not root.is_symlink(), 'projection directory')
    rows, total, visited = [], 0, 0
    pending = [root]
    while pending:
        parent = pending.pop()
        with os.scandir(parent) as entries:
            for entry in entries:
                visited += 1
                require(visited <= 8500, 'projection entry bound')
                path = Path(parent) / entry.name
                rel = path.relative_to(root).as_posix()
                info = entry.stat(follow_symlinks=False)
                if stat.S_ISDIR(info.st_mode):
                    require(rel in ('objects', 'refs', 'refs/heads') or re.fullmatch(r'objects/[0-9a-f]{2}', rel), 'projection directory path')
                    pending.append(path)
                    continue
                require(output_path(rel) and stat.S_ISREG(info.st_mode), 'unexpected projection file/kind')
                fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
                with os.fdopen(fd, 'rb') as source:
                    before = os.fstat(source.fileno())
                    require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1 and before.st_size <= 3 * 1024 * 1024, 'projection file kind/size')
                    data = source.read(3 * 1024 * 1024 + 1)
                    after = os.fstat(source.fileno())
                    named = path.lstat()
                    require((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
                            (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns)
                            and (before.st_dev, before.st_ino) == (named.st_dev, named.st_ino)
                            and len(data) == before.st_size, 'projection changed')
                total += len(data)
                require(total <= MAX_OUTPUT and len(rows) < 8192, 'projection aggregate bound')
                rows.append({'path': rel, 'sha256': sha(data), 'bytes': len(data)})
    require(any(row['path'] == 'HEAD' for row in rows) and any(row['path'] == 'index' for row in rows), 'incomplete projection')
    return sha(encoded(sorted(rows, key=lambda row: row['path'])))


def sandbox_argv(output):
    """Caller drops uid/gid, enters independently tracked cgroup, closes FDs."""
    require(Path(output).is_absolute(), 'projection output must be absolute')
    command = ['/usr/bin/bwrap', '--unshare-all', '--unshare-user', '--disable-userns',
               '--die-with-parent', '--new-session', '--cap-drop', 'ALL',
               '--ro-bind', '/usr', '/usr', '--symlink', 'usr/bin', '/bin',
               '--symlink', 'usr/sbin', '/sbin', '--symlink', 'usr/lib', '/lib',
               '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/etc',
               '--ro-bind', '/etc/ld.so.cache', '/etc/ld.so.cache',
               '--ro-bind', '/etc/alternatives', '/etc/alternatives',
               '--bind', str(output), '/output', '--chdir', '/tmp', '--clearenv']
    for key, value in ENV.items():
        command += ['--setenv', key, value]
    return command + ['--', '/usr/bin/python3', '-I', MODULE]


class Git:
    def __init__(self, directory):
        self.directory = directory
        self.deadline = time.monotonic() + 45
        self.commands = 0

    def run(self, args, data=b'', index=None, maximum=MAX_GIT_OUTPUT):
        require(time.monotonic() < self.deadline and self.commands < 20000, 'Git operation budget')
        self.commands += 1
        environment = {**ENV, 'GIT_DIR': str(self.directory)}
        if index is not None:
            environment['GIT_INDEX_FILE'] = str(index)
        # No filters, external diffs, hooks, replacements, remote helpers or
        # source configuration. All command verbs below are fixed plumbing.
        argv = ['/usr/bin/git', '-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false',
                '-c', 'protocol.allow=never', '-c', 'diff.external=', *args]
        with tempfile.TemporaryFile() as input_file:
            input_file.write(data)
            input_file.seek(0)
            child = subprocess.Popen(argv, stdin=input_file, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                     env=environment, close_fds=True, start_new_session=True)
            out, err = bytearray(), bytearray()
            reaped = False
            try:
                with selectors.DefaultSelector() as selector:
                    for stream, buffer in ((child.stdout, out), (child.stderr, err)):
                        os.set_blocking(stream.fileno(), False)
                        selector.register(stream, selectors.EVENT_READ, buffer)
                    until = min(self.deadline, time.monotonic() + 10)
                    while selector.get_map():
                        require(time.monotonic() < until, 'Git operation deadline')
                        for key, _ in selector.select(0.05):
                            chunk = os.read(key.fd, 65536)
                            if chunk:
                                require(len(out) + len(err) + len(chunk) <= maximum, 'Git output limit')
                                key.data.extend(chunk)
                            else:
                                selector.unregister(key.fileobj)
                child.wait(timeout=max(0.01, until - time.monotonic()))
                reaped = True
                require(child.returncode == 0, 'Git rejected snapshot')
                return bytes(out)
            finally:
                if not reaped and child.returncode is None:
                    try:
                        os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    child.wait(timeout=2)
                child.stdout.close()
                child.stderr.close()


def init(directory):
    (directory / 'objects').mkdir(parents=True)
    (directory / 'refs/heads').mkdir(parents=True)
    (directory / 'HEAD').write_bytes(HEAD)


def tree_entries(raw):
    require(not raw or raw.endswith(b'\0'), 'tree framing')
    entries, seen = [], set()
    for line in raw.rstrip(b'\0').split(b'\0') if raw else []:
        header, name = line.split(b'\t', 1)
        mode, kind, oid = header.decode('ascii').split(' ')
        name = relative(name.decode('utf-8'))
        require(mode in ('100644', '100755') and kind == 'blob' and OID.fullmatch(oid), 'unsupported HEAD mode/type')
        require(name not in seen and len(seen) < MAX_ENTRIES, 'HEAD duplicate/count')
        seen.add(name)
        if not excluded(name):
            entries.append((mode, oid, name))
    return entries


def materialize(snapshot, output):
    require(os.geteuid() != 0, 'Git projection must be unprivileged')
    head, files, index_entries = validate_snapshot(snapshot)
    output = Path(output)
    require(output.is_dir() and not output.is_symlink() and not any(output.iterdir()), 'output must be empty')
    with tempfile.TemporaryDirectory(prefix='xcb-git-') as temporary:
        raw = Path(temporary) / 'raw.git'
        init(raw)
        for name, data in files.items():
            target = raw / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        source = Git(raw)
        # Full validation includes blob contents and packed objects; connectivity
        # only would not detect corrupt blobs (git-scm.com/docs/git-fsck).
        source.run(['fsck', '--full', '--strict', '--no-reflogs', '--no-dangling'], maximum=1024 * 1024)
        if head is not None:
            require(source.run(['rev-parse', '--verify', 'HEAD'], maximum=1024).strip().decode() == head, 'HEAD binding')
            head_entries = tree_entries(source.run(['ls-tree', '-r', '-z', '--full-tree', head]))
        else:
            # A true unborn HEAD has no branch target, including in packed refs.
            ref = files['HEAD'].decode('ascii').strip()[5:]
            require(ref not in files and not any(line.endswith((' ' + ref).encode()) for line in files.get('packed-refs', b'').splitlines()), 'unborn HEAD binding')
            head_entries = []
        index_entries = [entry for entry in index_entries if not excluded(entry[2])]
        target = output / '.git'
        init(target)
        destination = Git(target)
        destination.deadline = source.deadline
        blobs = {}
        total = 0
        for _, oid, _ in head_entries + index_entries:
            if oid in blobs:
                continue
            data = source.run(['cat-file', 'blob', oid], maximum=MAX_FILE + 1)
            require(len(data) <= MAX_FILE and hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest() == oid, 'blob hash/size')
            total += len(data)
            require(total <= MAX_OUTPUT, 'filtered blob aggregate limit')
            written = destination.run(['hash-object', '-w', '--stdin'], data, maximum=1024).strip().decode()
            require(written == oid, 'materialized blob identity')
            blobs[oid] = True
        def populate(index_path, entries):
            destination.run(['read-tree', '--empty'], index=index_path, maximum=1024)
            body = b''.join((mode + ' ' + oid + '\t' + name).encode() + b'\0' for mode, oid, name in entries)
            if body:
                destination.run(['update-index', '-z', '--index-info'], body, index=index_path, maximum=1024)
        if head is not None:
            head_index = Path(temporary) / 'head.index'
            populate(head_index, head_entries)
            tree = destination.run(['write-tree'], index=head_index, maximum=1024).strip().decode()
            require(OID.fullmatch(tree), 'synthetic tree identity')
            commit = destination.run(['commit-tree', tree], MESSAGE, maximum=1024).strip().decode()
            require(OID.fullmatch(commit), 'synthetic commit identity')
            (target / BRANCH).write_text(commit + '\n')
        populate(target / 'index', index_entries)
        projection = validate_output(output)
        return {'version': 1, 'syntheticHead': True, 'headFiles': len(head_entries),
                'indexFiles': len(index_entries), 'projectionSha256': projection}


def main():
    require(sys.platform == 'linux' and os.geteuid() == UID, 'confined guest projection required')
    os.umask(0o077)
    resource.setrlimit(resource.RLIMIT_AS, (768 * 1024 * 1024, 768 * 1024 * 1024))
    resource.setrlimit(resource.RLIMIT_CPU, (45, 45))
    resource.setrlimit(resource.RLIMIT_FSIZE, (MAX_OUTPUT, MAX_OUTPUT))
    resource.setrlimit(resource.RLIMIT_NOFILE, (128, 128))
    raw = sys.stdin.buffer.read(MAX_INPUT + 1)
    require(len(raw) <= MAX_INPUT, 'input envelope limit')
    result = materialize(decode(raw), Path('/output'))
    sys.stdout.buffer.write(encoded(result) + b'\n')


if __name__ == '__main__':
    try:
        main()
    except BaseException:
        print('xcb Git projection refused', file=sys.stderr)
        sys.exit(1)
