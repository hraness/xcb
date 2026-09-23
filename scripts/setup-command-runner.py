#!/usr/bin/env python3
"""Provision only a separately owned XCB Lima home. Run under hra-host-run."""
import argparse
import fcntl
import hashlib
import json
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import time

GIB = 1024 ** 3
LIMA = '/opt/homebrew/bin/limactl'
RUST_COMPONENTS = {
    'rustc': 'b344b81f0cd4c2246c7da8b197fe7a339d7dd02bb15cb69b2524115d9c75224c',
    'rust-std': '46aed8e63186350004d8ec6afca798811e6530b514352e5a8a26f3dc4939b3be',
    'cargo': '8f70bcaccea5ba4db187c3fd4d64e24592b4e16af513497201f5909d61691dbe',
    'rustfmt': '3dbde15d30794924195ae446f3d2ceb542a131306d22ae7912c7634d414622a8',
    'clippy': 'd8bac7b0ba5ca9bb868ccb9e367a1d52f4837f3ebf4892eaf64cda37ce362bb5',
}
NODE_SHA256 = '7201e3a09dc825bac57867c81913e2b8f0ef87d04cb9082af4cda82f6ff3d88c'
BUN_SHA256 = 'a27ffb63a8310375836e0d6f668ae17fa8d8d18b88c37c821c65331973a19a3b'

def toolchain_script():
    # Fixed, checksum-pinned public release inputs. No project/package scripts,
    # ambient shell configuration or credential-bearing host paths are read.
    lines = ['set -eu', 'umask 077', 'cd /opt/xcb-tools',
             'stage=$(mktemp -d /opt/xcb-tools/install.XXXXXXXX)',
             'trap \'rm -rf "$stage"\' EXIT HUP INT TERM',
             'cd "$stage"']
    def download(url, checksum, filename):
        lines.append('curl --fail --location --proto =https --proto-redir =https --tlsv1.2 --max-time 300 --max-filesize 140000000 -o ' + filename + ' ' + url)
        lines.append('printf "%s  %s\\n" ' + checksum + ' ' + filename + ' | sha256sum --check --status')
    for component, checksum in RUST_COMPONENTS.items():
        stem = component + '-1.97.1-aarch64-unknown-linux-gnu'
        download('https://static.rust-lang.org/dist/2026-07-16/' + stem + '.tar.xz', checksum, 'component.tar.xz')
        lines.extend(['tar --no-same-owner -xf component.tar.xz', './' + stem + '/install.sh --prefix=/opt/xcb-tools --disable-ldconfig',
                      'rm -rf component.tar.xz ' + stem])
    download('https://nodejs.org/dist/v24.18.1/node-v24.18.1-linux-arm64.tar.xz', NODE_SHA256, 'node.tar.xz')
    lines.extend(['tar --no-same-owner -xf node.tar.xz -C /opt/xcb-tools',
                  'for name in node npm npx; do ln -sfn ../node-v24.18.1-linux-arm64/bin/$name /opt/xcb-tools/bin/$name; done'])
    download('https://github.com/oven-sh/bun/releases/download/bun-v1.3.14/bun-linux-aarch64.zip', BUN_SHA256, 'bun.zip')
    lines.extend(['unzip -q bun.zip', 'install -m 0555 bun-linux-aarch64/bun /opt/xcb-tools/bin/bun',
                  'chmod -R go-w /opt/xcb-tools'])
    return ('\n'.join(lines) + '\n').encode()

def sha(data):
    return hashlib.sha256(data).hexdigest()

def private_dir(path):
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    s = path.lstat()
    if path.resolve() != path or not path.is_dir() or s.st_uid != os.getuid() or s.st_mode & 0o077:
        raise RuntimeError('unsafe private command root')

def create(path, data):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())

def main():
    p = argparse.ArgumentParser()
    p.add_argument('--root', type=pathlib.Path, required=True)
    p.add_argument('--source', type=pathlib.Path, default=pathlib.Path(__file__).resolve().parents[1])
    p.add_argument('--refresh', action='store_true', help='replace a stopped backend source identity, preserving the old manifest')
    args = p.parse_args()
    root = args.root
    if not root.is_absolute() or root == pathlib.Path.home() / '.lima':
        raise RuntimeError('select a distinct absolute XCB-owned command root')
    private_dir(root)
    marker = root / 'xcb-owner.json'
    expected = b'{"owner":"xcb-command-v1"}\n'
    if marker.exists():
        if marker.read_bytes() != expected:
            raise RuntimeError('foreign command root')
    else:
        if list(root.iterdir()):
            raise RuntimeError('nonempty unowned command root')
        create(marker, expected)
    for name in ('lima', 'cache', 'home', 'jobs'):
        private_dir(root / name)
    admission_fd = os.open(root / 'admission.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    admission_metadata = os.fstat(admission_fd)
    if not stat.S_ISREG(admission_metadata.st_mode) or admission_metadata.st_uid != os.getuid() or admission_metadata.st_nlink != 1 or admission_metadata.st_mode & 0o077:
        raise RuntimeError('unsafe command admission lock')
    fcntl.flock(admission_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    # Retain this descriptor throughout setup. An old runtime may not start a
    # command while its immutable guest helper or qualification is changing.
    jobs_dir = root / 'jobs'
    pending_dir = jobs_dir / 'pending'
    if pending_dir.exists():
        # Mirror the runtime's pending-marker admission: once the migration
        # sentinel exists only marked jobs can hold unjoined custody, so the
        # scan stays proportional to pending work. A partial migration is
        # refused rather than trusted; the next runtime admission rebuilds it.
        if not pending_dir.is_dir() or not (pending_dir / 'pending-ready').is_file():
            raise RuntimeError('incomplete pending-marker migration; retry after one runtime admission')
        candidates = []
        for marker in pending_dir.iterdir():
            if marker.name == 'pending-ready':
                continue
            job = jobs_dir / marker.name
            if not marker.is_file() or not job.is_dir():
                raise RuntimeError('pending marker without its job record')
            candidates.append(job)
    else:
        candidates = [job for job in jobs_dir.iterdir() if job.name != 'pending']
    for job in candidates:
        if (job / 'started.json').exists():
            custody = json.loads((job / 'custody.json').read_bytes())
            joined = False
            for name in ('outcome.json', 'recovered.json'):
                if (job / name).exists():
                    outcome = json.loads((job / name).read_bytes())
                    joined |= outcome.get('custody') == custody and outcome.get('joined') is True
            if not joined:
                raise RuntimeError('unjoined command custody prevents setup; recover it first')
    env = {'HOME': str(root / 'home'), 'LIMA_HOME': str(root / 'lima'),
           'XDG_CACHE_HOME': str(root / 'cache'), 'SSH': '/usr/bin/ssh',
           'PATH': '/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin', 'LANG': 'en_US.UTF-8'}
    source = args.source.resolve()
    guest = (source / 'crates/xcb-runtime/src/command/guest.py').read_bytes()
    projection_path = source / 'crates/xcb-runtime/src/command/git_projection.py'
    projection = projection_path.read_bytes()
    cache_path = source / 'crates/xcb-runtime/src/command/public_cache.py'
    cache_module = cache_path.read_bytes()
    projection_test_path = source / 'crates/xcb-runtime/src/command/test_git_projection.py'
    projection_test = projection_test_path.read_bytes()
    policy = (source / 'crates/xcb-runtime/src/command/lima.yaml').read_bytes()
    qualifier_path = source / 'crates/xcb-runtime/src/command/test_live.py'
    qualifier = qualifier_path.read_bytes()
    config = root / 'lima.yaml'
    resize = False
    if config.exists():
        if config.read_bytes() != policy:
            if args.refresh and config.read_bytes() == policy.replace(b'disk: 8GiB',b'disk: 4GiB'):
                resize = True
            else:
                raise RuntimeError('provisioning configuration changed; no automatic VM mutation')
    else:
        create(config, policy)
    def allocated():
        return sum(path.lstat().st_blocks * 512 for path in root.rglob('*'))
    initial_bytes = allocated()
    if shutil.disk_usage(root).free < 8 * GIB + max(0, 6 * GIB - initial_bytes):
        raise RuntimeError('requires remaining own provisioning budget plus 8GiB free-space floor')
    def guard():
        if shutil.disk_usage(root).free < 8 * GIB or allocated() > 6 * GIB:
            raise RuntimeError('provisioning disk guard reached; preserve state for diagnosis')
    def run(argv, data=None, timeout=900):
        guard()
        command = subprocess.Popen(argv, stdin=subprocess.PIPE if data is not None else subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT, env=env)
        # This setup only executes fixed trusted provisioning commands; the
        # outer HRA wrapper owns the full process tree and the VM is dedicated.
        try:
            output, _ = command.communicate(data, timeout=timeout)
        except subprocess.TimeoutExpired:
            command.terminate()
            try:
                output, _ = command.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                command.kill()
                output, _ = command.communicate(timeout=5)
            sys.stderr.buffer.write(output[-12000:])
            raise RuntimeError('trusted provisioning operation timed out; VM state retained')
        log = root / ('setup-' + str(time.time_ns()) + '.log')
        create(log, output)
        guard()
        if command.returncode:
            sys.stderr.buffer.write(output[-12000:])
            raise RuntimeError('provisioning command failed')
        return output
    if (root / 'lima/worker').exists():
        if (root / 'lima/worker/lima.yaml').read_bytes() != (policy.replace(b'disk: 8GiB',b'disk: 4GiB') if resize else policy):
            raise RuntimeError('existing VM policy changed')
        run([LIMA, 'start', '--tty=false', '--timeout=12m', 'worker'])
    else:
        run([LIMA, 'start', '--tty=false', '--name=worker', '--timeout=12m', str(config)])
    prefix = [LIMA, 'shell', '--workdir', '/', 'worker', 'sudo', '-n']
    active = run(prefix + ['/bin/sh', '-c', 'systemctl list-units \"xcb-command-*.service\" --state=activating,active,deactivating --no-legend --no-pager']).strip()
    if active:
        raise RuntimeError('command units remain active; preserve backend custody')
    # A failed prelaunch may have staged a filesystem but never reached the
    # irreversible child-attempt marker. Bind its helper source to the exact
    # retained manifest before asking the reviewed supervisor to reconcile it.
    pending_code = '''import pathlib,json
root=pathlib.Path('/var/lib/xcb-command/jobs')
rows=[]
preloads=pathlib.Path('/var/lib/xcb-command/preloads')
for preload in preloads.iterdir() if preloads.exists() else []:
 result=preload/'recovered.json' if (preload/'recovered.json').exists() else preload/'result.json'
 assert result.exists() and json.loads(result.read_bytes()).get('joined') is True, 'dependency preparation remains pending; recover it before setup'
for job in root.iterdir():
 if not (job/'result.json').exists():
  launch=job/'launch.json'
  source=json.loads(launch.read_bytes())['guestSha256'] if launch.exists() else None
  rows.append({'custody':json.loads((job/'custody.json').read_bytes()),'guestSha256':source})
assert len(rows)<=64
print(json.dumps(rows))
'''
    pending=json.loads(run(prefix+['/usr/bin/python3','-c',pending_code]))
    for entry in pending:
        custody=entry['custody']; key=custody['backendSha256']
        if len(key)!=64 or any(c not in '0123456789abcdef' for c in key):
            raise RuntimeError('invalid pending backend identity')
        originals=[root/'backend.json',root/('backend-'+key+'.json'),root/('candidate-'+key+'.json')]
        matches=[json.loads(path.read_bytes()) for path in originals if path.exists() and sha(path.read_bytes())==key]
        if not matches or any(match!=matches[0] for match in matches) or matches[0]['bootId']!=custody['bootId']:
            raise RuntimeError('pending prelaunch lacks its exact original manifest; custody retained')
        if entry['guestSha256'] is not None and matches[0]['guestSha256']!=entry['guestSha256']:
            raise RuntimeError('pending prelaunch source differs from its original manifest')
        # Legacy source comes from the immutable original manifest, never the
        # currently installed helper (which an interrupted refresh may replace).
        entry['guestSha256']=matches[0]['guestSha256']
    if resize:
        run(prefix + ['/usr/bin/python3', '-c', 'import json,pathlib; jobs=pathlib.Path("/var/lib/xcb-command/jobs"); assert all(json.loads((job/"result.json").read_bytes())["joined"] is True for job in jobs.iterdir()), "unjoined command prevents VM resize"'])
        run([LIMA,'stop','--tty=false','worker'],timeout=120)
        run([LIMA,'edit','--tty=false','--disk=8','worker'])
        run([LIMA,'edit','--tty=false','--disk=8',str(config)])
        if config.read_bytes() != policy or (root/'lima/worker/lima.yaml').read_bytes()!=policy:
            raise RuntimeError('VM resize changed unrelated policy')
        run([LIMA,'start','--tty=false','--timeout=12m','worker'])
    if (root / 'backend.json').exists() and not args.refresh:
        existing = json.loads((root / 'backend.json').read_bytes())
        if existing['guestSha256'] != sha(guest):
            raise RuntimeError('backend source changed; explicit --refresh required')
    run(prefix + ['/bin/sh', '-c', 'umask 077; cat > /usr/local/lib/xcb-command/guest.py.new && chmod 0555 /usr/local/lib/xcb-command/guest.py.new && mv /usr/local/lib/xcb-command/guest.py.new /usr/local/lib/xcb-command/guest.py'], guest)
    run(prefix + ['/bin/sh', '-c', 'umask 077; cat > /usr/local/lib/xcb-command/git_projection.py.new && chmod 0555 /usr/local/lib/xcb-command/git_projection.py.new && mv /usr/local/lib/xcb-command/git_projection.py.new /usr/local/lib/xcb-command/git_projection.py'], projection)
    run(prefix + ['/bin/sh', '-c', 'umask 077; cat > /usr/local/lib/xcb-command/public_cache.py.new && chmod 0555 /usr/local/lib/xcb-command/public_cache.py.new && mv /usr/local/lib/xcb-command/public_cache.py.new /usr/local/lib/xcb-command/public_cache.py'], cache_module)
    for entry in pending:
        identifier=entry['custody']['commandId']
        raw=run(prefix+['/usr/bin/python3','/usr/local/lib/xcb-command/guest.py','recover-prelaunch',identifier],json.dumps(entry,separators=(',',':')).encode())
        result=json.loads(raw)
        if result.get('custody')!=entry['custody'] or result.get('unstarted') is not True or result.get('joined') is not True:
            raise RuntimeError('prelaunch recovery did not prove a definitive no-start')
        retained=root/('prelaunch-'+identifier+'.json')
        if retained.exists():
            if retained.read_bytes()!=raw:raise RuntimeError('prelaunch recovery receipt changed')
        else:create(retained,raw)
        ack={'version':1,'custody':entry['custody'],'resultSha256':sha(raw),'hostReceiptSha256':sha(raw)}
        run(prefix+['/usr/bin/python3','/usr/local/lib/xcb-command/guest.py','ack',identifier],json.dumps(ack,separators=(',',':')).encode())
    recipe = toolchain_script()
    stamp = '/opt/xcb-tools/xcb-toolchain-' + sha(recipe) + '.sha256'
    installed = run(prefix + ['/bin/sh','-c','if test -f "$1" && sha256sum --check --status "$1"; then printf verified; fi','xcb-toolchain-check',stamp])
    if installed != b'verified':
        run(prefix + ['/bin/sh'], recipe, timeout=1800)
        # Pin the installed executables after the reviewed archive installers;
        # explicit refresh detects drift instead of trusting a version string.
        run(prefix + ['/bin/sh','-c','set -eu; umask 077; sha256sum /opt/xcb-tools/bin/rustc /opt/xcb-tools/bin/cargo /opt/xcb-tools/bin/rustfmt /opt/xcb-tools/bin/clippy-driver /opt/xcb-tools/bin/node /opt/xcb-tools/bin/bun > "$1"','xcb-toolchain-stamp',stamp])
    observation = json.loads(run(prefix + ['/usr/bin/python3', '/usr/local/lib/xcb-command/guest.py', 'inspect']))
    versions = run(prefix + ['/bin/sh', '-c', '/usr/bin/python3 --version; /opt/xcb-tools/bin/node --version; /opt/xcb-tools/bin/rustc --version; /opt/xcb-tools/bin/bun --version']).decode().splitlines()
    manifest = {'version': 1, 'limaExecutable': str(pathlib.Path(LIMA).resolve()),
                'limaSha256': sha(pathlib.Path(LIMA).read_bytes()), 'guestSha256': sha(guest),
                'policySha256': sha(policy), 'gitProjectionSha256': sha(projection), 'gitProjectionTestsSha256': sha(projection_test), 'publicCacheSha256': sha(cache_module), 'bootId': observation['bootId'],
                'bwrapSha256': observation['bwrapSha256'], 'toolVersions': versions,
                'toolDigests': observation['toolDigests'],
                'bounds': {'snapshotBytes': 96 * 1024 * 1024, 'workspaceBytes': 2 * 1024 * 1024 * 1024,
                           'outputBytes': 256 * 1024, 'changesBytes': 24 * 1024 * 1024,
                           'maximumTimeoutMs': 600000, 'maximumTasks': 256,
                           'workerMemoryBytes': 1536 * 1024 * 1024, 'cacheBytes': 1024 * 1024 * 1024, 'network': 'none'}}
    require_sha = observation['agentSha256'] == manifest['guestSha256'] and observation['gitProjectionSha256'] == manifest['gitProjectionSha256'] and observation['publicCacheSha256'] == manifest['publicCacheSha256']
    if not require_sha:
        raise RuntimeError('guest agent mismatch')
    environment = json.dumps(manifest, sort_keys=True, separators=(',', ':')).encode()
    candidate = root / ('candidate-' + sha(environment) + '.json')
    if candidate.exists():
        if candidate.read_bytes() != environment:
            raise RuntimeError('candidate manifest changed')
    else:
        create(candidate, environment)
    evidence_path = root / ('qualification-pending-' + str(time.time_ns()) + '.json')
    run([sys.executable, str(qualifier_path), '--root', str(root),
         '--candidate-manifest', str(candidate), '--output', str(evidence_path)], timeout=1800)
    evidence_bytes = evidence_path.read_bytes()
    evidence = json.loads(evidence_bytes)
    required_cases = ['file-edit-and-python', 'uid-filesystem-network-and-userns',
                      'detached-setsid-closed-stdio', 'deadline-kills-descendants',
                      'output-overflow-joins', 'pre-cancel-never-executes',
                      'offline-language-toolchains', 'peer-work-and-control-denied',
                      'git-projection-unit-semantics', 'readonly-filtered-git-inspection',
                      'public-cache-isolation-and-key-binding', 'offline-cargo-bun-cache-usage']
    if len(evidence_bytes) > 2 * 1024 * 1024 or evidence['version'] != 1 or evidence['environmentSha256'] != sha(environment) or evidence['suiteSha256'] != sha(qualifier) or [case['name'] for case in evidence['cases']] != required_cases:
        raise RuntimeError('complete exact boundary qualification is required')
    if cache_path.read_bytes() != cache_module or projection_path.read_bytes() != projection or projection_test_path.read_bytes() != projection_test or qualifier_path.read_bytes() != qualifier or (source / 'crates/xcb-runtime/src/command/guest.py').read_bytes() != guest or (source / 'crates/xcb-runtime/src/command/lima.yaml').read_bytes() != policy:
        raise RuntimeError('qualification inputs changed during setup')
    evidence_target = root / ('qualification-' + sha(evidence_bytes) + '.json')
    create(evidence_target, evidence_bytes)
    manifest['qualification'] = {'suiteSha256': sha(qualifier), 'environmentSha256': sha(environment),
                                 'evidenceSha256': sha(evidence_bytes)}
    target = root / 'backend.json'
    encoded = json.dumps(manifest, sort_keys=True, separators=(',', ':')).encode()
    if target.exists() and target.read_bytes() != encoded:
        if not args.refresh:
            raise RuntimeError('backend identity changed; explicit --refresh required')
        original = target.read_bytes()
        backup = root / ('backend-' + sha(original) + '.json')
        if not backup.exists():
            create(backup, original)
        elif backup.read_bytes() != original:
            raise RuntimeError('backend backup mismatch')
        staged = root / ('backend-next-' + str(time.time_ns()) + '.json')
        create(staged, encoded)
        os.replace(staged, target)
    elif not target.exists():
        create(target, encoded)
    print(json.dumps({'ready': True, 'root': str(root), 'backendSha256': sha(encoded),
                      'freeBytes': shutil.disk_usage(root).free, 'toolVersions': versions,
                      'qualifiedCases': len(required_cases)}))
    os.close(admission_fd)

if __name__ == '__main__':
    main()
