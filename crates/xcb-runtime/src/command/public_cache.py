#!/usr/bin/python3
"""Fixed public dependency preload recipe; every phase runs unprivileged.

Supervisor contract: fresh bounded output, 1GiB filesystem budget, independent
cgroup/pipe custody and complete joining before accepting output. `plan` and
`materialize` have no external network; only `fetch` shares the VM network.
No consumer directory, SSH state, credentials or ambient configuration is bound.
The worker receives only the final immutable cache, never fetch staging.
"""
import base64
import fcntl
import hashlib
import http.client
import http.server
import io
import json
import os
from pathlib import Path
import re
import resource
import selectors
import signal
import socket
import ssl
import stat
import subprocess
import struct
import sys
import tarfile
import tempfile
import threading
import time
from urllib.parse import quote

UID = 61001
MODULE = '/usr/local/lib/xcb-command/public_cache.py'
LIMIT = 1024 * 1024 * 1024
MAX_ARCHIVE = 128 * 1024 * 1024
MAX_INPUT = 16 * 1024 * 1024
MAX_FILES = 200000
TOOLS = ('bun', 'cargo', 'git', 'node', 'python', 'rustc')
NAME = re.compile(r'^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,127}$')
VERSION = re.compile(r'^[0-9][a-zA-Z0-9.+-]{0,127}$')
SHA = re.compile(r'^[0-9a-f]{64}$')
GIT_SOURCE = re.compile(r'^git\+https://github\.com/([A-Za-z0-9][A-Za-z0-9_.-]{0,99})/([A-Za-z0-9][A-Za-z0-9_.-]{0,99})(?:\?rev=([0-9a-f]{40}))?#([0-9a-f]{40})$')
REGISTRY = 'registry+https://github.com/rust-lang/crates.io-index'
ENV = {'PATH': '/opt/xcb-tools/bin:/usr/bin:/bin', 'HOME': '/tmp/home', 'LANG': 'C.UTF-8',
       'TMPDIR': '/tmp', 'CARGO_HOME': '/tmp/cargo', 'RUSTUP_HOME': '/opt/xcb-tools/rustup',
       'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null',
       'GIT_TERMINAL_PROMPT': '0', 'GIT_ASKPASS': '/bin/false', 'SSH_ASKPASS': '/bin/false',
       'GIT_OPTIONAL_LOCKS': '0', 'GIT_NO_REPLACE_OBJECTS': '1', 'GIT_NO_LAZY_FETCH': '1',
       'GIT_LFS_SKIP_SMUDGE': '1', 'GIT_ATTR_NOSYSTEM': '1', 'BUN_CONFIG_NO_CLEAR_TERMINAL': '1'}


class Refused(ValueError):
    """A guard refusal, never an external exception body."""


SAFE_REFUSALS = frozenset((
    'tool user namespace mapping', 'tool overflow owner', 'tool readonly mount',
    'tool root', 'tool custody', 'tool identity changed',
    'preload process deadline', 'preload process output limit',
    'preload tool rejected locked inputs', 'public archive unavailable; redirects are refused',
    'archive length', 'archive byte limit', 'download deadline',
    'lockfile archive checksum mismatch', 'public Git commit identity mismatch',
    'Git submodules are unsupported', 'fresh fetch output required',
    'archive entry count', 'archive prefix', 'archive root', 'relative path',
    'archive link/special/duplicate refused', 'unpacked file size', 'unpacked aggregate size',
    'truncated tar member', 'verified crate drift', 'Cargo vendor configuration',
    'vendor source keys', 'vendor directory binding', 'vendor Git binding',
    'frozen Bun lock drift', 'local package server did not join',
    'fetch plan differs from exact manifests/tools', 'fresh materialization output required',
    'materializer requires isolated loopback-only namespace', 'materializer loopback must be up',
    'fresh scratch and output must share the bounded filesystem',
    'cache directory', 'cache entry count', 'cache symlink refused', 'cache link escape',
    'cache file kind/size', 'cache aggregate size', 'cache changed',
    'Bun cache alias root', 'Bun cache alias count', 'Bun cache alias target',
    'Bun cache alias changed',
))


SAFE_DIAGNOSTIC_FUNCTIONS = frozenset((
    'main', 'materialize', 'cargo_materialize', 'bun_materialize', 'verify_loopback',
    'unpack', 'inventory', 'normalize_bun_links', 'run', 'verify_tools', 'verify_tool', 'confined_tool_owner',
    'kernel_text', 'plan', 'inputs', 'validate_plan', 'fetch', 'download',
))


def refusal_site(error):
    # Only names from this pinned trusted module and a numeric source position.
    # Never serialize a traceback, filename, source line, local or foreign frame.
    trace, site = error.__traceback__, ''
    for _ in range(64):
        if trace is None:
            break
        code = trace.tb_frame.f_code
        if code.co_filename == __file__ and code.co_name in SAFE_DIAGNOSTIC_FUNCTIONS:
            site = ' [' + code.co_name + ':' + str(trace.tb_lineno) + ']'
        trace = trace.tb_next
    return site


def refusal_message(error):
    if type(error) is Refused and str(error) in SAFE_REFUSALS:
        detail = str(error)
    elif isinstance(error, ssl.SSLCertVerificationError):
        detail = 'TLS certificate validation failed'
    elif isinstance(error, ssl.SSLError):
        detail = 'TLS transport failed'
    elif isinstance(error, TimeoutError):
        detail = 'bounded operation timed out'
    elif isinstance(error, OSError):
        detail = 'filesystem or network operation failed'
    else:
        detail = 'input or preload operation rejected'
    return 'xcb public locked dependency preload refused: ' + detail + refusal_site(error)


def require(value, message):
    if not value:
        raise Refused(message)


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


def relative(value):
    require(isinstance(value, str) and 0 < len(value.encode()) <= 4096, 'path length')
    parts = value.split('/')
    require(len(parts) <= 64 and all(p and p not in ('.', '..', '.git') for p in parts), 'relative path')
    require(not any(ord(c) < 32 or ord(c) == 127 for c in value), 'path controls')
    return value


def jsonc(raw):
    """Bun text lockfiles: permit trailing commas, never evaluate JavaScript."""
    text = raw.decode('utf-8')
    output, string, escape, i = [], False, False, 0
    while i < len(text):
        char = text[i]
        if string:
            output.append(char)
            if escape:
                escape = False
            elif char == '\\':
                escape = True
            elif char == '"':
                string = False
            i += 1
            continue
        if char == '"':
            string = True
        if char == ',':
            j = i + 1
            while j < len(text) and text[j].isspace():
                j += 1
            if j < len(text) and text[j] in '}]':
                i += 1
                continue
        # Current admitted Bun1.3 text format does not require comments.
        output.append(char)
        i += 1
    return decode(''.join(output).encode())


def npm_identity(descriptor):
    require(isinstance(descriptor, str) and '@' in descriptor, 'npm identity')
    name, version = descriptor.rsplit('@', 1)
    parts = name[1:].split('/') if name.startswith('@') else [name]
    require(len(parts) == (2 if name.startswith('@') else 1) and all(NAME.fullmatch(p) for p in parts)
            and VERSION.fullmatch(version), 'public npm package identity')
    return name, version


def artifact(kind, name, version, checksum):
    if kind == 'crate':
        require(NAME.fullmatch(name) and VERSION.fullmatch(version) and SHA.fullmatch(checksum), 'crate identity/checksum')
        url = 'https://static.crates.io/crates/' + name + '/' + name + '-' + version + '.crate'
        integrity = {'algorithm': 'sha256', 'digest': checksum}
    else:
        require(kind == 'npm', 'archive kind')
        npm_identity(name + '@' + version)
        require(isinstance(checksum, str) and checksum.startswith('sha512-'), 'npm SHA512 required')
        expected = base64.b64decode(checksum[7:], validate=True)
        require(len(expected) == 64, 'npm checksum length')
        integrity = {'algorithm': 'sha512', 'digest': expected.hex()}
        url = 'https://registry.npmjs.org/' + quote(name, safe='@/') + '/-/' + name.rsplit('/', 1)[-1] + '-' + quote(version, safe='.-') + '.tgz'
    row = {'kind': kind, 'name': name, 'version': version, 'url': url, 'integrity': integrity}
    return {**row, 'id': sha(encoded(row))}


def git_artifact(source):
    match = GIT_SOURCE.fullmatch(source) if isinstance(source, str) else None
    require(match is not None, 'only exact public GitHub commit sources are admitted')
    owner, repository, revision, commit = match.groups()
    require(repository not in ('.', '..') and not repository.endswith('.git') and (revision is None or revision == commit), 'Git source identity')
    row = {'kind': 'git', 'url': 'https://github.com/' + owner + '/' + repository,
           'commit': commit, 'source': source}
    return {**row, 'id': sha(encoded(row))}


def inputs(spec):
    closed(spec, ('version', 'toolDigests', 'files'))
    require(type(spec['version']) is int and spec['version'] == 1, 'input version')
    closed(spec['toolDigests'], TOOLS)
    require(all(isinstance(v, str) and SHA.fullmatch(v) for v in spec['toolDigests'].values()), 'tool identities')
    require(isinstance(spec['files'], list) and 1 <= len(spec['files']) <= 256, 'input file count')
    files, total = {}, 0
    for row in spec['files']:
        closed(row, ('path', 'base64', 'sha256'))
        path = relative(row['path'])
        require(Path(path).name in ('Cargo.toml', 'Cargo.lock', 'package.json', 'bun.lock'), 'only manifests and lockfiles')
        require(path not in files and isinstance(row['base64'], str) and len(row['base64']) <= 3 * 1024 * 1024, 'manifest encoding')
        data = base64.b64decode(row['base64'], validate=True)
        require(len(data) <= 2 * 1024 * 1024 and sha(data) == row['sha256'], 'manifest digest/size')
        total += len(data)
        require(total <= 8 * 1024 * 1024, 'manifest aggregate')
        files[path] = data
    return files


def plan(spec):
    import tomllib  # Guest Python3.11+ only; host plan validation stays stdlib3.9.
    files = inputs(spec)
    archives, repositories = {}, {}
    cargo = 'Cargo.lock' in files
    bun = 'bun.lock' in files
    require(cargo or bun, 'root lockfile required')
    require(not any(Path(p).name in ('Cargo.lock', 'bun.lock') and '/' in p for p in files), 'one root lockfile per ecosystem')
    for path, data in files.items():
        if path.endswith('Cargo.toml'):
            require(isinstance(tomllib.loads(data.decode()), dict), 'Cargo manifest')
        elif path.endswith('package.json'):
            require(isinstance(decode(data), dict), 'package manifest')
    if cargo:
        require('Cargo.toml' in files, 'Cargo root manifest required')
        lock = tomllib.loads(files['Cargo.lock'].decode())
        require(lock.get('version') in (3, 4) and isinstance(lock.get('package'), list) and len(lock['package']) <= 2048, 'Cargo lock format/count')
        for package in lock['package']:
            require(isinstance(package, dict) and NAME.fullmatch(package.get('name', '')) and VERSION.fullmatch(package.get('version', '')), 'Cargo package')
            source = package.get('source')
            if source is None:
                continue
            if source == REGISTRY:
                row = artifact('crate', package['name'], package['version'], package.get('checksum', ''))
                archives[row['id']] = row
            else:
                row = git_artifact(source)
                repositories[row['id']] = row
    if bun:
        require('package.json' in files, 'Bun root manifest required')
        lock = jsonc(files['bun.lock'])
        require(lock.get('lockfileVersion') == 1 and isinstance(lock.get('packages'), dict) and len(lock['packages']) <= 2048, 'Bun lock format/count')
        for key, row in lock['packages'].items():
            require(isinstance(row, list) and len(row) == 4 and row[1] == '' and isinstance(row[2], dict), 'only public npm registry entries')
            name, version = npm_identity(row[0])
            # Platform-incompatible optional packages are unnecessary in this VM.
            compatible = True
            for field, target in (('os', 'linux'), ('cpu', 'arm64')):
                value = row[2].get(field)
                if value is not None:
                    values = [value] if isinstance(value, str) else value
                    require(isinstance(values, list) and all(isinstance(v, str) for v in values), 'package platform')
                    compatible &= target not in [v[1:] for v in values if v.startswith('!')] and (all(v.startswith('!') for v in values) or target in values)
            if compatible:
                entry = artifact('npm', name, version, row[3])
                archives[entry['id']] = entry
    rows = [{'path': path, 'sha256': sha(data)} for path, data in sorted(files.items())]
    identity = {'version': 1, 'platform': 'linux-arm64', 'preloaderSha256': sha(Path(__file__).read_bytes()),
                'toolDigests': spec['toolDigests'], 'inputs': rows, 'cargo': cargo, 'bun': bun,
                'archives': sorted(archives.values(), key=lambda v: v['id']),
                'repositories': sorted(repositories.values(), key=lambda v: v['id'])}
    return {**identity, 'cacheKey': sha(encoded(identity))}


def validate_plan(value):
    closed(value, ('version', 'platform', 'preloaderSha256', 'toolDigests', 'inputs', 'cargo', 'bun', 'cacheKey', 'archives', 'repositories'))
    identity = {key: value[key] for key in value if key != 'cacheKey'}
    require(value['version'] == 1 and value['platform'] == 'linux-arm64' and value['preloaderSha256'] == sha(Path(__file__).read_bytes())
            and value['cacheKey'] == sha(encoded(identity)), 'plan identity')
    closed(value['toolDigests'], TOOLS)
    require(all(isinstance(v, str) and SHA.fullmatch(v) for v in value['toolDigests'].values()), 'plan tools')
    require(type(value['cargo']) is bool and type(value['bun']) is bool and (value['cargo'] or value['bun']), 'plan ecosystems')
    require(isinstance(value['inputs'], list) and len(value['inputs']) <= 256, 'plan inputs')
    for row in value['inputs']:
        closed(row, ('path', 'sha256'))
        relative(row['path'])
        require(Path(row['path']).name in ('Cargo.toml', 'Cargo.lock', 'package.json', 'bun.lock') and SHA.fullmatch(row['sha256']), 'plan input identity')
    require(isinstance(value['archives'], list) and len(value['archives']) <= 4096 and isinstance(value['repositories'], list) and len(value['repositories']) <= 128, 'artifact counts')
    seen = set()
    for row in value['archives']:
        closed(row, ('kind', 'name', 'version', 'url', 'integrity', 'id'))
        closed(row['integrity'], ('algorithm', 'digest'))
        expected = row['integrity']['digest'] if row['kind'] == 'crate' else 'sha512-' + base64.b64encode(bytes.fromhex(row['integrity']['digest'])).decode()
        require(artifact(row['kind'], row['name'], row['version'], expected) == row and row['id'] not in seen, 'fixed archive source')
        seen.add(row['id'])
    for row in value['repositories']:
        closed(row, ('kind', 'url', 'commit', 'source', 'id'))
        require(git_artifact(row['source']) == row and row['id'] not in seen, 'fixed Git source')
        seen.add(row['id'])
    return value


def kernel_text(path, maximum):
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC), 'rb') as stream:
        value = stream.read(maximum + 1)
    require(len(value) <= maximum, 'tool user namespace mapping')
    return value.decode('ascii')


def confined_tool_owner():
    # Root's cache_identity/inspect attests UID0 ownership before dropping UID.
    # bwrap's nested namespace maps only UID61001 to its parent's UID0,
    # itself mapped to the unprivileged worker. Host root is therefore unmapped
    # and stat represents the attested owner by kernel overflowuid. Both tool
    # trees remain read-only; overflow ownership alone is never authority.
    mapping = kernel_text('/proc/self/uid_map', 256).splitlines()
    require(os.geteuid() == UID and len(mapping) == 1
            and mapping[0].split() == [str(UID), '0', '1'], 'tool user namespace mapping')
    overflow = kernel_text('/proc/sys/kernel/overflowuid', 32).strip()
    require(overflow.isascii() and overflow.isdecimal() and 0 < int(overflow) < 2 ** 32 - 1
            and int(overflow) != UID, 'tool overflow owner')
    return int(overflow)


def tool_stamp(metadata):
    return (metadata.st_dev, metadata.st_ino, metadata.st_mode, metadata.st_uid,
            metadata.st_nlink, metadata.st_size, metadata.st_mtime_ns, metadata.st_ctime_ns)


def verify_tool(selected, expected, owner):
    path = selected.resolve(strict=True)
    require(path.is_relative_to('/usr') or path.is_relative_to('/opt/xcb-tools'), 'tool root')
    require(os.statvfs(path).f_flag & os.ST_RDONLY, 'tool readonly mount')
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    with os.fdopen(fd, 'rb') as stream:
        before = os.fstat(stream.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_uid == owner and before.st_mode & 0o022 == 0
                and before.st_mode & 0o111 and before.st_size <= 256 * 1024 * 1024, 'tool custody')
        require(tool_stamp(before) == tool_stamp(path.stat()), 'tool identity changed')
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        require(digest == expected and tool_stamp(before) == tool_stamp(os.fstat(stream.fileno()))
                == tool_stamp(path.stat()) and selected.resolve(strict=True) == path, 'tool identity changed')


def verify_tools(value):
    owner = confined_tool_owner()
    for name in TOOLS:
        selected = Path('/usr/bin/git') if name == 'git' else Path('/usr/bin/python3') if name == 'python' else Path('/opt/xcb-tools/bin') / name
        verify_tool(selected, value['toolDigests'][name], owner)


def run(argv, deadline, data=b'', cwd=None, extra=None, maximum=2 * 1024 * 1024):
    environment = {**ENV, **(extra or {})}
    with tempfile.TemporaryFile() as source:
        source.write(data)
        source.seek(0)
        child = subprocess.Popen(argv, stdin=source, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 cwd=cwd, env=environment, close_fds=True, start_new_session=True)
        output, error, reaped = bytearray(), bytearray(), False
        try:
            with selectors.DefaultSelector() as selector:
                for stream, buffer in ((child.stdout, output), (child.stderr, error)):
                    os.set_blocking(stream.fileno(), False)
                    selector.register(stream, selectors.EVENT_READ, buffer)
                while selector.get_map():
                    require(time.monotonic() < deadline, 'preload process deadline')
                    for key, _ in selector.select(0.05):
                        chunk = os.read(key.fd, 65536)
                        if chunk:
                            require(len(output) + len(error) + len(chunk) <= maximum, 'preload process output limit')
                            key.data.extend(chunk)
                        else:
                            selector.unregister(key.fileobj)
            child.wait(timeout=max(0.01, deadline - time.monotonic()))
            reaped = True
            require(child.returncode == 0, 'preload tool rejected locked inputs')
            return bytes(output)
        finally:
            if not reaped and child.returncode is None:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                child.wait(timeout=3)
            child.stdout.close()
            child.stderr.close()


def git_command(directory, *args):
    return ['/usr/bin/git', '-c', 'credential.helper=', '-c', 'core.hooksPath=/dev/null',
            '-c', 'core.fsmonitor=false', '-c', 'http.followRedirects=false',
            '-c', 'protocol.allow=never', '-c', 'protocol.https.allow=always',
            '-c', 'submodule.recurse=false', '--git-dir=' + str(directory), *args]


def download(row, destination, remaining, deadline):
    # row has already been re-derived by validate_plan. Never honor a proxy,
    # redirect, alternate origin, registry token or caller-provided URL.
    host = 'static.crates.io' if row['kind'] == 'crate' else 'registry.npmjs.org'
    prefix = 'https://' + host
    require(row['url'].startswith(prefix + '/'), 'download origin')
    context = ssl.create_default_context(cafile='/etc/ssl/certs/ca-certificates.crt')
    connection = http.client.HTTPSConnection(host, 443, timeout=20, context=context)
    digest = hashlib.new(row['integrity']['algorithm'])
    count = 0
    try:
        connection.request('GET', row['url'][len(prefix):], headers={'Accept': 'application/octet-stream', 'User-Agent': 'xcb-public-cache/1'})
        response = connection.getresponse()
        require(response.status == 200, 'public archive unavailable; redirects are refused')
        length = response.getheader('Content-Length')
        require(length is None or length.isdecimal() and int(length) <= min(MAX_ARCHIVE, remaining), 'archive length')
        with destination.open('xb') as output:
            while True:
                require(time.monotonic() < deadline, 'download deadline')
                chunk = response.read(65536)
                if not chunk:
                    break
                count += len(chunk)
                require(count <= min(MAX_ARCHIVE, remaining), 'archive byte limit')
                digest.update(chunk)
                output.write(chunk)
        require(digest.hexdigest() == row['integrity']['digest'], 'lockfile archive checksum mismatch')
    finally:
        connection.close()
    return count


def inventory(root, allow_links=False):
    root = Path(root)
    require(root.is_dir() and not root.is_symlink(), 'cache directory')
    rows, total, pending = [], 0, [root]
    while pending:
        directory = pending.pop()
        with os.scandir(directory) as entries:
            for entry in entries:
                require(len(rows) + len(pending) < MAX_FILES, 'cache entry count')
                path = Path(directory) / entry.name
                name = path.relative_to(root).as_posix()
                info = entry.stat(follow_symlinks=False)
                if stat.S_ISDIR(info.st_mode):
                    pending.append(path)
                    rows.append({'path': name, 'kind': 'directory'})
                elif stat.S_ISLNK(info.st_mode):
                    require(allow_links, 'cache symlink refused')
                    target = os.readlink(path)
                    require(not Path(target).is_absolute() and len(target.encode()) <= 4096 and path.resolve().is_relative_to(root.resolve()), 'cache link escape')
                    rows.append({'path': name, 'kind': 'symlink', 'target': target})
                else:
                    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
                    with os.fdopen(fd, 'rb') as stream:
                        before = os.fstat(stream.fileno())
                        require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1 and before.st_size <= LIMIT, 'cache file kind/size')
                        digest = hashlib.sha256()
                        size = 0
                        while chunk := stream.read(1024 * 1024):
                            size += len(chunk)
                            total += len(chunk)
                            require(total <= LIMIT, 'cache aggregate size')
                            digest.update(chunk)
                        after = os.fstat(stream.fileno())
                        named = path.lstat()
                        require((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
                                (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns)
                                and (before.st_dev, before.st_ino) == (named.st_dev, named.st_ino) and size == before.st_size, 'cache changed')
                    rows.append({'path': name, 'kind': 'file', 'bytes': size, 'executable': bool(before.st_mode & 0o111), 'sha256': digest.hexdigest()})
    return {'sha256': sha(encoded(sorted(rows, key=lambda row: row['path']))), 'bytes': total, 'entries': len(rows)}


def fetch(value, output):
    value = validate_plan(value)
    verify_tools(value)
    output = Path(output)
    require(output.is_dir() and not any(output.iterdir()), 'fresh fetch output required')
    (output / 'archives').mkdir()
    (output / 'git').mkdir()
    deadline, total = time.monotonic() + 600, 0
    for row in value['archives']:
        total += download(row, output / 'archives' / row['id'], LIMIT - total, deadline)
    for row in value['repositories']:
        directory = output / 'git' / (row['id'] + '.git')
        # No checkout, templates, hooks, submodules, credential helpers or SSH.
        run(git_command(directory, 'init', '--bare', '--template='), deadline)
        run(git_command(directory, 'fetch', '--quiet', '--no-tags', '--no-recurse-submodules', row['url'], row['commit']), deadline)
        actual = run(git_command(directory, 'rev-parse', '--verify', 'FETCH_HEAD^{commit}'), deadline, maximum=1024).strip().decode()
        require(actual == row['commit'], 'public Git commit identity mismatch')
        run(git_command(directory, 'fsck', '--full', '--strict', '--no-reflogs', '--no-dangling'), deadline)
        tree = run(git_command(directory, 'ls-tree', '-r', '--name-only', '-z', row['commit']), deadline)
        require(not any(Path(name.decode()).name == '.gitmodules' for name in tree.split(b'\0') if name), 'Git submodules are unsupported')
        run(git_command(directory, 'update-ref', 'refs/heads/xcb-source', row['commit']), deadline)
        run(git_command(directory, 'symbolic-ref', 'HEAD', 'refs/heads/xcb-source'), deadline)
        total = inventory(output)['bytes']
    (output / 'plan.json').write_bytes(encoded(value))
    receipt = inventory(output)
    return {'version': 1, 'cacheKey': value['cacheKey'], 'fetched': True, 'inventory': receipt}


def unpack(data, target, prefix, budget):
    target.mkdir(parents=True)
    seen = set()
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
        for member in archive:
            require(len(seen) < MAX_FILES, 'archive entry count')
            raw = member.name.rstrip('/')
            require(raw == prefix or raw.startswith(prefix + '/'), 'archive prefix')
            if raw == prefix:
                require(member.isdir(), 'archive root')
                continue
            name = relative(raw[len(prefix) + 1:])
            require(name not in seen and (member.isfile() or member.isdir()), 'archive link/special/duplicate refused')
            seen.add(name)
            path = target / name
            if member.isdir():
                path.mkdir(parents=True, exist_ok=True)
                continue
            require(0 <= member.size <= MAX_ARCHIVE, 'unpacked file size')
            budget[0] += member.size
            require(budget[0] <= LIMIT, 'unpacked aggregate size')
            path.parent.mkdir(parents=True, exist_ok=True)
            with archive.extractfile(member) as source, path.open('xb') as output:
                remaining = member.size
                while remaining:
                    chunk = source.read(min(65536, remaining))
                    require(chunk, 'truncated tar member')
                    output.write(chunk)
                    remaining -= len(chunk)
            path.chmod(0o755 if member.mode & 0o111 else 0o644)


def cargo_materialize(value, files, fetched, output, scratch, deadline):
    import tomllib
    registry = scratch / 'registry'
    registry.mkdir()
    budget = [0]
    for row in value['archives']:
        if row['kind'] != 'crate':
            continue
        data = (fetched / 'archives' / row['id']).read_bytes()
        require(hashlib.sha256(data).hexdigest() == row['integrity']['digest'], 'verified crate drift')
        crate = registry / (row['name'] + '-' + row['version'])
        unpack(data, crate, row['name'] + '-' + row['version'], budget)
        checksums = {p.relative_to(crate).as_posix(): sha(p.read_bytes()) for p in crate.rglob('*') if p.is_file()}
        (crate / '.cargo-checksum.json').write_bytes(encoded({'package': row['integrity']['digest'], 'files': checksums}))
    workspace = scratch / 'workspace'
    workspace.mkdir()
    for path, data in files.items():
        if Path(path).name not in ('Cargo.lock', 'Cargo.toml'):
            continue
        target = workspace / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        if path.endswith('Cargo.toml'):
            manifest = tomllib.loads(data.decode())
            if 'package' in manifest:
                # Dependency discovery only. No source or build script executes.
                for implicit in ('src/lib.rs', 'src/main.rs'):
                    dummy = target.parent / implicit
                    dummy.parent.mkdir(parents=True, exist_ok=True)
                    dummy.write_bytes(b'')
    config = scratch / 'cargo-config.toml'
    config.write_text('[source.crates-io]\nreplace-with="xcb-registry"\n[source.xcb-registry]\ndirectory=' + json.dumps(str(registry)) + '\n')
    gitconfig = scratch / 'gitconfig'
    gitconfig.write_text('[credential]\nhelper=\n[core]\nhooksPath=/dev/null\n[protocol]\nallow=never\n[protocol "file"]\nallow=always\n[submodule]\nrecurse=false\n' + ''.join(
        '[url ' + json.dumps('file://' + str(fetched / 'git' / (row['id'] + '.git'))) + ']\ninsteadOf=' + row['url'] + '\n' for row in value['repositories']))
    cargo = output / 'cargo'
    cargo.mkdir()
    result = run(['/opt/xcb-tools/bin/cargo', '--config', str(config), 'vendor', '--locked', '--versioned-dirs', '--respect-source-config', str(cargo / 'vendor')], deadline,
                 cwd=workspace, extra={'GIT_CONFIG_GLOBAL': str(gitconfig), 'CARGO_NET_GIT_FETCH_WITH_CLI': 'true'})
    parsed = tomllib.loads(result.decode())
    require(set(parsed) == {'source'} and isinstance(parsed['source'], dict), 'Cargo vendor configuration')
    # Cargo emits only replacement source definitions; admit the exact output
    # shape, then relocate its one directory into the readonly worker cache.
    for name, source in parsed['source'].items():
        require(isinstance(source, dict) and set(source) <= {'replace-with', 'directory', 'git', 'rev', 'branch', 'tag', 'registry'}, 'vendor source keys')
        if 'directory' in source:
            require(source == {'directory': str(cargo / 'vendor')}, 'vendor directory binding')
        if 'git' in source:
            require(any(source['git'] == row['url'] and source.get('rev', row['commit']) == row['commit'] for row in value['repositories']), 'vendor Git binding')
    text = result.decode().replace(str(cargo / 'vendor'), '/opt/xcb-cache/cargo/vendor')
    (cargo / 'config.toml').write_text(text)


def normalize_bun_links(cache):
    # Bun has joined and the local archive server has stopped. Its lookup index
    # uses absolute in-cache aliases; convert only verified existing targets so
    # the final read-only cache can move from /output to /opt/xcb-cache.
    cache = Path(cache)
    require(cache.is_dir() and not cache.is_symlink(), 'Bun cache alias root')
    cache = cache.resolve(strict=True)
    root = cache.stat()
    pending, links, count = [cache], [], 0
    while pending:
        directory = pending.pop()
        with os.scandir(directory) as entries:
            for entry in entries:
                count += 1
                require(count <= MAX_FILES, 'Bun cache alias count')
                path, before = Path(entry.path), entry.stat(follow_symlinks=False)
                if stat.S_ISDIR(before.st_mode):
                    pending.append(path)
                elif stat.S_ISLNK(before.st_mode):
                    links.append((path, before, os.readlink(path)))
    for path, before, original in links:
        require(len(original.encode()) <= 4096, 'Bun cache alias target')
        try:
            target = path.resolve(strict=True)
        except (OSError, RuntimeError):
            raise Refused('Bun cache alias target') from None
        require(target.is_relative_to(cache) and target not in path.parents
                and (target.is_file() or target.is_dir()), 'Bun cache alias target')
        target_before = target.stat()
        require(tool_stamp(path.lstat()) == tool_stamp(before) and os.readlink(path) == original,
                'Bun cache alias changed')
        if not Path(original).is_absolute():
            continue
        relative = os.path.relpath(target, path.parent)
        temporary = path.with_name('.xcb-cache-link-' + os.urandom(16).hex())
        os.symlink(relative, temporary)
        replacement = temporary.lstat()
        require(tool_stamp(path.lstat()) == tool_stamp(before) and os.readlink(path) == original
                and tool_stamp(target.stat()) == tool_stamp(target_before)
                and (cache.stat().st_dev, cache.stat().st_ino) == (root.st_dev, root.st_ino),
                'Bun cache alias changed')
        os.replace(temporary, path)
        # rename updates ctime on supported kernels; inode/mode/owner/size and
        # mtime must still match our exact freshly created replacement.
        require(tool_stamp(path.lstat())[:-1] == tool_stamp(replacement)[:-1] and os.readlink(path) == relative
                and path.resolve(strict=True) == target, 'Bun cache alias changed')


def bun_materialize(value, files, fetched, output, scratch, deadline):
    routes = {}
    for row in value['archives']:
        if row['kind'] == 'npm':
            routes[row['url'][len('https://registry.npmjs.org'):]] = row
    class Handler(http.server.BaseHTTPRequestHandler):
        def setup(self):
            super().setup()
            self.connection.settimeout(2)
        def do_GET(self):
            row = routes.get(self.path)
            if row is None:
                self.send_error(404)
                return
            data = (fetched / 'archives' / row['id']).read_bytes()
            if hashlib.sha512(data).hexdigest() != row['integrity']['digest']:
                self.send_error(500)
                return
            self.send_response(200)
            self.send_header('Content-Length', str(len(data)))
            self.send_header('Content-Type', 'application/octet-stream')
            self.end_headers()
            self.wfile.write(data)
        def log_message(self, *_):
            pass
    server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
    server_thread = threading.Thread(target=server.serve_forever)
    server_thread.start()
    try:
        workspace = scratch / 'bun-workspace'
        workspace.mkdir()
        for path, data in files.items():
            if Path(path).name in ('bun.lock', 'package.json'):
                target = workspace / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
        cache = output / 'bun'
        cache.mkdir()
        run(['/opt/xcb-tools/bin/bun', 'install', '--frozen-lockfile', '--ignore-scripts', '--network-concurrency', '1', '--registry', 'http://127.0.0.1:' + str(server.server_port)], deadline,
            cwd=workspace, extra={'BUN_INSTALL_CACHE_DIR': str(cache)})
        require((workspace / 'bun.lock').read_bytes() == files['bun.lock'], 'frozen Bun lock drift')
    finally:
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=3)
        require(not server_thread.is_alive(), 'local package server did not join')
    normalize_bun_links(cache)


def materialize(spec, fetched, output):
    files = inputs(spec)
    value = plan(spec)
    verify_tools(value)
    fetched, output = Path(fetched), Path(output)
    require(decode((fetched / 'plan.json').read_bytes()) == value, 'fetch plan differs from exact manifests/tools')
    require(output.is_dir() and not any(output.iterdir()), 'fresh materialization output required')
    deadline = time.monotonic() + 600
    with tempfile.TemporaryDirectory(prefix='xcb-public-cache-') as temporary:
        scratch = Path(temporary)
        if value['cargo']:
            cargo_materialize(value, files, fetched, output, scratch, deadline)
        if value['bun']:
            bun_materialize(value, files, fetched, output, scratch, deadline)
    # All package-manager children and local archive-server threads have joined.
    result = inventory(output, allow_links=True)
    return {'version': 1, 'cacheKey': value['cacheKey'], 'complete': True, 'inventory': result}


def sandbox_argv(output, scratch, phase, fetched=None, private_loopback=False):
    """Caller owns cgroup and one bounded filesystem for output AND scratch.

    For materialize, the trusted root supervisor first creates a fresh network
    namespace, raises only lo, then drops all groups/uid/capabilities before
    calling this argv. --share-net shares that isolated namespace, never VMnet.
    Runtime kernel checks below reject accidental use in the VM's namespace.
    """
    require(phase in ('plan', 'fetch', 'materialize') and Path(output).is_absolute()
            and Path(scratch).is_absolute() and Path(output) != Path(scratch), 'preload phase/output/scratch')
    require((fetched is not None) == (phase == 'materialize'), 'materialization input')
    require(private_loopback == (phase == 'materialize'), 'private loopback namespace required')
    argv = ['/usr/bin/bwrap', '--unshare-all', '--unshare-user', '--disable-userns', '--die-with-parent', '--new-session', '--cap-drop', 'ALL',
            '--ro-bind', '/usr', '/usr', '--ro-bind', '/opt/xcb-tools', '/opt/xcb-tools',
            '--symlink', 'usr/bin', '/bin', '--symlink', 'usr/sbin', '/sbin', '--symlink', 'usr/lib', '/lib',
            '--proc', '/proc', '--dev', '/dev', '--bind', str(scratch), '/tmp', '--dir', '/etc',
            '--ro-bind', '/etc/ld.so.cache', '/etc/ld.so.cache', '--ro-bind', '/etc/alternatives', '/etc/alternatives',
            '--bind', str(output), '/output', '--chdir', '/tmp', '--clearenv']
    if phase == 'fetch':
        argv += ['--share-net', '--ro-bind', '/etc/resolv.conf', '/etc/resolv.conf',
                 '--ro-bind', '/etc/ssl/certs', '/etc/ssl/certs']
    if fetched is not None:
        require(Path(fetched).is_absolute(), 'absolute verified fetch directory')
        argv += ['--share-net', '--ro-bind', str(fetched), '/input']
    for key, value in ENV.items():
        argv += ['--setenv', key, value]
    return argv + ['--', '/usr/bin/python3', '-I', MODULE, phase]


def verify_loopback():
    require([name for _, name in socket.if_nameindex()] == ['lo'], 'materializer requires isolated loopback-only namespace')
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        flags = fcntl.ioctl(probe.fileno(), 0x8913, struct.pack('256s', b'lo'))
    require(struct.unpack('H', flags[16:18])[0] & 1, 'materializer loopback must be up')


def main():
    require(sys.platform == 'linux' and os.geteuid() == UID and len(sys.argv) == 2, 'confined preload required')
    phase = sys.argv[1]
    require(phase in ('plan', 'fetch', 'materialize'), 'preload phase')
    os.umask(0o077)
    require(os.stat('/tmp').st_dev == os.stat('/output').st_dev and not any(Path('/tmp').iterdir()),
            'fresh scratch and output must share the bounded filesystem')
    if phase == 'materialize':
        verify_loopback()
    Path('/tmp/home').mkdir(exist_ok=True)
    # Bun/JSC reserves a large virtual arena. The supervisor's cgroup bounds
    # physical memory; a low inherited RLIMIT_AS would reject a healthy runtime.
    resource.setrlimit(resource.RLIMIT_FSIZE, (LIMIT, LIMIT))
    resource.setrlimit(resource.RLIMIT_NOFILE, (256, 256))
    raw = sys.stdin.buffer.read(MAX_INPUT + 1)
    require(len(raw) <= MAX_INPUT, 'preload envelope limit')
    value = decode(raw)
    if phase == 'plan':
        result = plan(value)
    elif phase == 'fetch':
        result = fetch(value, Path('/output'))
    else:
        result = materialize(value, Path('/input'), Path('/output'))
    sys.stdout.buffer.write(encoded(result) + b'\n')


if __name__ == '__main__':
    try:
        main()
    except BaseException as error:
        print(refusal_message(error), file=sys.stderr)
        sys.exit(1)
