#!/usr/bin/python3
"""Hermetic planner/archive/custody contracts; no downloads or package tools."""
import base64
import hashlib
import importlib.util
import io
import json
import os
import ssl
import stat
from types import SimpleNamespace
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('public_cache', Path(__file__).with_name('public_cache.py'))
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)

COMMIT = '1' * 40
SOURCE = 'git+https://github.com/example/library?rev=' + COMMIT + '#' + COMMIT
CRATE = b'synthetic checksum-bound crate'
NPM = b'synthetic checksum-bound npm tarball'


def dto(files):
    return {'version': 1, 'toolDigests': {name: 'a' * 64 for name in p.TOOLS},
            'files': [{'path': path, 'base64': base64.b64encode(data).decode(), 'sha256': p.sha(data)} for path, data in files.items()]}


def fixture(git=True):
    cargo = 'version = 4\n[[package]]\nname = "sample"\nversion = "1.0.0"\nsource = "' + p.REGISTRY + '"\nchecksum = "' + p.sha(CRATE) + '"\n'
    if git:
        cargo += '[[package]]\nname = "library"\nversion = "0.1.0"\nsource = "' + SOURCE + '"\n'
    integrity = 'sha512-' + base64.b64encode(hashlib.sha512(NPM).digest()).decode()
    bun = {'lockfileVersion': 1, 'packages': {'sample': ['sample@1.0.0', '', {}, integrity]}}
    return dto({'Cargo.toml': b'[package]\nname="root"\nversion="1.0.0"\n', 'Cargo.lock': cargo.encode(),
                'package.json': b'{"name":"root","scripts":{"postinstall":"touch forbidden"}}', 'bun.lock': json.dumps(bun).encode()})


def tar(entries):
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode='w:gz') as archive:
        for name, kind, content in entries:
            info = tarfile.TarInfo(name)
            if kind == 'file':
                info.size = len(content)
                archive.addfile(info, io.BytesIO(content))
            else:
                info.type = tarfile.SYMTYPE if kind == 'symlink' else tarfile.LNKTYPE
                info.linkname = content
                archive.addfile(info)
    return data.getvalue()


class PublicCacheTests(unittest.TestCase):
    def test_plan_binds_exact_inputs_module_tools_and_public_sources(self):
        value = fixture()
        plan = p.plan(value)
        self.assertEqual(p.validate_plan(plan), plan)
        self.assertEqual(len(plan['archives']), 2)
        self.assertEqual(plan['repositories'][0]['commit'], COMMIT)
        self.assertNotIn('touch forbidden', json.dumps(plan))
        self.assertEqual(p.plan({**value, 'files': list(reversed(value['files']))})['cacheKey'], plan['cacheKey'])
        changed = json.loads(json.dumps(value))
        changed['toolDigests']['bun'] = 'b' * 64
        self.assertNotEqual(p.plan(changed)['cacheKey'], plan['cacheKey'])
        files = p.inputs(value)
        files['package.json'] += b'\n'
        self.assertNotEqual(p.plan(dto(files))['cacheKey'], plan['cacheKey'])

    def test_no_credentials_custom_registries_ssh_redirects_or_unpinned_git(self):
        for source in ['git+ssh://git@github.com/example/library#' + COMMIT,
                       'git+https://token@github.com/example/library#' + COMMIT,
                       'git+https://github.com/example/library?branch=main#' + COMMIT,
                       'git+https://github.com/example/library?rev=' + '2' * 40 + '#' + COMMIT,
                       'git+https://localhost/example/library#' + COMMIT,
                       'registry+https://private.example/index', 42]:
            with self.assertRaises(ValueError):
                p.git_artifact(source)
        plan = p.plan(fixture())
        plan['archives'][0]['url'] = 'https://registry.npmjs.org@private.example/secret'
        with self.assertRaises(ValueError):
            p.validate_plan(plan)
        command = p.git_command(Path('/output/git'), 'fetch', '--no-recurse-submodules', 'https://github.com/example/library', COMMIT)
        self.assertIn('http.followRedirects=false', command)
        self.assertIn('credential.helper=', command)
        self.assertIn('protocol.allow=never', command)
        self.assertIn('core.hooksPath=/dev/null', command)
        self.assertNotIn('SSH_AUTH_SOCK', p.ENV)
        self.assertNotIn('HTTPS_PROXY', p.ENV)

    def test_manifest_closed_bounds_and_digest_checks(self):
        for files in [{'../Cargo.lock': b''}, {'.npmrc': b'token=secret'}, {'nested/.git/package.json': b'{}'}]:
            with self.assertRaises(ValueError):
                p.inputs(dto(files))
        value = fixture()
        value['files'][0]['sha256'] = '0' * 64
        with self.assertRaises(ValueError):
            p.plan(value)
        value = fixture()
        value['files'].append(value['files'][0].copy())
        with self.assertRaises(ValueError):
            p.plan(value)
        with self.assertRaises(ValueError):
            p.plan({**fixture(), 'credentials': 'never accepted'})
        with self.assertRaises(ValueError):
            p.decode(b'{"a":1,"a":2}')

    def test_bun_trailing_commas_preserve_strings_and_reject_code(self):
        self.assertEqual(p.jsonc(b'{"a":["comma,}",],}'), {'a': ['comma,}']})
        for raw in [b'{"a": process.env.TOKEN}', b'{"a":1,//comment\n}', b'{"a":1,"a":2,}']:
            with self.assertRaises(ValueError):
                p.jsonc(raw)

    def test_npm_integrity_and_linux_arm64_selection(self):
        files = p.inputs(fixture(False))
        lock = p.decode(files['bun.lock'])
        row = lock['packages']['sample']
        for name, platform in [('darwin', {'os': 'darwin'}), ('x64', {'cpu': 'x64'}), ('arm', {'cpu': 'arm64', 'os': 'linux'})]:
            lock['packages'][name] = [name + '@1.0.0', '', platform, row[3]]
        files['bun.lock'] = p.encoded(lock)
        names = [a['name'] for a in p.plan(dto(files))['archives'] if a['kind'] == 'npm']
        self.assertEqual(set(names), {'sample', 'arm'})
        lock['packages']['sample'][1] = 'https://private.example/package.tgz'
        files['bun.lock'] = p.encoded(lock)
        with self.assertRaises(ValueError):
            p.plan(dto(files))
        with self.assertRaises(ValueError):
            p.artifact('npm', 'name', '1.0.0', 'sha1-' + 'a' * 40)

    def test_sandbox_network_is_separate_and_no_home_workspace_or_auth_mounts(self):
        for phase in ['plan', 'fetch', 'materialize']:
            argv = p.sandbox_argv('/safe/output', '/safe/scratch', phase, '/safe/fetched' if phase == 'materialize' else None, phase == 'materialize')
            self.assertEqual('--share-net' in argv, phase in ('fetch', 'materialize'))
            self.assertIn('--unshare-all', argv)
            self.assertIn('--clearenv', argv)
            self.assertNotIn('--tmpfs', argv)
            self.assertIn('/safe/scratch', argv)
            self.assertNotIn('/home', argv)
            self.assertNotIn('/work', argv)
            self.assertEqual(argv[-2:], [p.MODULE, phase])
        with self.assertRaises(ValueError):
            p.sandbox_argv('relative', '/safe/scratch', 'fetch')
        with self.assertRaises(ValueError):
            p.sandbox_argv('/safe/output', '/safe/scratch', 'materialize', '/safe/fetched')

    def test_materializer_kernel_guard_rejects_external_interfaces_and_down_loopback(self):
        with patch.object(p.socket, 'if_nameindex', return_value=[(1, 'lo'), (2, 'eth0')]), self.assertRaises(ValueError):
            p.verify_loopback()
        for up in (False, True):
            data = b'\0' * 16 + (b'\1\0' if up else b'\0\0') + b'\0' * 238
            with patch.object(p.socket, 'if_nameindex', return_value=[(1, 'lo')]), patch.object(p.socket, 'socket'), patch.object(p.fcntl, 'ioctl', return_value=data):
                if up:
                    p.verify_loopback()
                else:
                    with self.assertRaises(ValueError):
                        p.verify_loopback()

    def test_archive_extraction_refuses_escapes_links_duplicates_and_bounds(self):
        cases = [[('sample-1.0.0/../escape', 'file', b'x')],
                 [('sample-1.0.0/link', 'symlink', '/etc/passwd')],
                 [('sample-1.0.0/link', 'hardlink', 'sample-1.0.0/file')],
                 [('other/file', 'file', b'x')],
                 [('sample-1.0.0/file', 'file', b'x'), ('sample-1.0.0/file', 'file', b'y')]]
        for entries in cases:
            with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
                p.unpack(tar(entries), Path(tmp) / 'crate', 'sample-1.0.0', [0])
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / 'crate'
            p.unpack(tar([('sample-1.0.0/src/lib.rs', 'file', b'pub fn x() {}')]), target, 'sample-1.0.0', [0])
            self.assertEqual((target / 'src/lib.rs').read_bytes(), b'pub fn x() {}')
        with tempfile.TemporaryDirectory() as tmp, patch.object(p, 'LIMIT', 1), self.assertRaises(ValueError):
            p.unpack(tar([('sample-1.0.0/file', 'file', b'xx')]), Path(tmp) / 'crate', 'sample-1.0.0', [0])

    def test_inventory_rejects_hardlinks_and_external_symlinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'file').write_bytes(b'cache')
            initial = p.inventory(root)
            self.assertEqual(initial['bytes'], 5)
            os.link(root / 'file', root / 'alias')
            with self.assertRaises(ValueError):
                p.inventory(root)
            (root / 'alias').unlink()
            (root / 'alias').symlink_to('/etc/passwd')
            with self.assertRaises(ValueError):
                p.inventory(root, allow_links=True)
            (root / 'alias').unlink()
            (root / 'alias').symlink_to('file')
            self.assertNotEqual(p.inventory(root, allow_links=True)['sha256'], initial['sha256'])

    def test_fetch_git_recipe_never_checks_out_or_runs_scripts(self):
        value = p.plan(fixture())
        calls = []
        def fake_download(row, destination, *_):
            body = CRATE if row['kind'] == 'crate' else NPM
            destination.write_bytes(body)
            return len(body)
        def fake_run(argv, *_args, **_kwargs):
            calls.append(argv)
            gitdir = Path(next(a.split('=', 1)[1] for a in argv if a.startswith('--git-dir=')))
            gitdir.mkdir(exist_ok=True)
            if 'rev-parse' in argv:
                return (COMMIT + '\n').encode()
            if 'ls-tree' in argv:
                return b'Cargo.toml\0src/lib.rs\0'
            return b''
        with tempfile.TemporaryDirectory() as tmp, patch.object(p, 'verify_tools'), patch.object(p, 'download', fake_download), patch.object(p, 'run', fake_run):
            result = p.fetch(value, Path(tmp))
            self.assertTrue(result['fetched'])
            self.assertEqual(result['cacheKey'], value['cacheKey'])
            self.assertEqual(p.decode((Path(tmp) / 'plan.json').read_bytes()), value)
        self.assertTrue(any('fsck' in c for c in calls))
        self.assertTrue(any('FETCH_HEAD^{commit}' in c for c in calls))
        self.assertTrue(all('checkout' not in c and 'submodule' not in c and '/bin/sh' not in c for c in calls))

    def test_fetch_commit_mismatch_never_returns_success(self):
        value = p.plan(fixture())
        value['archives'] = []
        value['cacheKey'] = p.sha(p.encoded({k: v for k, v in value.items() if k != 'cacheKey'}))
        with tempfile.TemporaryDirectory() as tmp, patch.object(p, 'verify_tools'), patch.object(p, 'run', return_value=b'wrong\n'), self.assertRaises(ValueError):
            p.fetch(value, Path(tmp))


    def test_tool_owner_requires_exact_unprivileged_namespace_and_kernel_overflow(self):
        for uid in (65534, 65533):
            with patch.object(p.os, 'geteuid', return_value=p.UID), patch.object(p, 'kernel_text', side_effect=['     61001          0          1\n', str(uid) + '\n']):
                self.assertEqual(p.confined_tool_owner(), uid)
        for mapping in ['0 0 4294967295\n', '61001 0 2\n', '61001 61001 1\n', '61001 0 1\n0 0 1\n', '']:
            with self.subTest(mapping=mapping), patch.object(p.os, 'geteuid', return_value=p.UID), patch.object(p, 'kernel_text', return_value=mapping), self.assertRaisesRegex(p.Refused, '^tool user namespace mapping$'):
                p.confined_tool_owner()
        with patch.object(p.os, 'geteuid', return_value=0), patch.object(p, 'kernel_text', return_value='61001 0 1\n'), self.assertRaises(p.Refused):
            p.confined_tool_owner()
        for owner in ['0', '61001', '4294967295', '-1', 'secret', '65534 1']:
            with self.subTest(owner=owner), patch.object(p.os, 'geteuid', return_value=p.UID), patch.object(p, 'kernel_text', side_effect=['61001 0 1\n', owner]), self.assertRaisesRegex(p.Refused, '^tool overflow owner$'):
                p.confined_tool_owner()

    def test_tool_guard_keeps_readonly_mode_digest_and_full_identity_checks(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'tool'
            body = b'public fixed executable'
            path.write_bytes(body)
            path.chmod(0o755)
            actual = path.stat()
            fields = ('st_dev', 'st_ino', 'st_mode', 'st_uid', 'st_nlink', 'st_size', 'st_mtime_ns', 'st_ctime_ns')
            before = SimpleNamespace(**{name: getattr(actual, name) for name in fields})
            before.st_uid = 65534
            def check(metadata=before, mounted=True, expected=None, changed=None):
                states = [metadata, changed or metadata]
                with patch.object(p.Path, 'is_relative_to', return_value=True), patch.object(p.os, 'statvfs', return_value=SimpleNamespace(f_flag=p.os.ST_RDONLY if mounted else 0)), patch.object(p.os, 'fstat', side_effect=states), patch.object(p.Path, 'stat', return_value=metadata):
                    p.verify_tool(path, expected or p.sha(body), 65534)
            check()
            with self.assertRaisesRegex(p.Refused, '^tool readonly mount$'):
                check(mounted=False)
            for changes in [{'st_uid': 0}, {'st_uid': p.UID}, {'st_mode': stat.S_IFREG | 0o777}, {'st_mode': stat.S_IFREG | 0o644}, {'st_mode': stat.S_IFIFO | 0o755}, {'st_size': 256 * 1024 * 1024 + 1}]:
                metadata = SimpleNamespace(**{**vars(before), **changes})
                with self.subTest(changes=changes), self.assertRaisesRegex(p.Refused, '^tool custody$'):
                    check(metadata)
            with self.assertRaisesRegex(p.Refused, '^tool identity changed$'):
                check(expected='0' * 64)
            for field in fields:
                metadata = SimpleNamespace(**{**vars(before), field: getattr(before, field) + 1})
                with self.subTest(changed=field), self.assertRaisesRegex(p.Refused, '^tool identity changed$'):
                    check(changed=metadata)
            with self.assertRaisesRegex(p.Refused, '^tool root$'):
                p.verify_tool(path, p.sha(body), 65534)

    def test_tool_guard_uses_only_fixed_paths_and_parent_attested_digests(self):
        value = {'toolDigests': {name: str(index) * 64 for index, name in enumerate(p.TOOLS)}}
        with patch.object(p, 'confined_tool_owner', return_value=65534), patch.object(p, 'verify_tool') as verify:
            p.verify_tools(value)
        expected = [(Path('/usr/bin/git') if name == 'git' else Path('/usr/bin/python3') if name == 'python' else Path('/opt/xcb-tools/bin') / name, value['toolDigests'][name], 65534) for name in p.TOOLS]
        self.assertEqual([call.args for call in verify.call_args_list], expected)

    def test_fixed_diagnostics_never_emit_external_exception_bodies(self):
        for error in [p.Refused('secret arbitrary caller bytes'), ValueError('secret'), OSError('secret private path'), ssl.SSLError('secret remote response'), ssl.SSLCertVerificationError('secret certificate'), TimeoutError('secret URL')]:
            result = p.refusal_message(error)
            self.assertNotIn('secret', result)
            self.assertLess(len(result), 160)
            self.assertTrue(result.startswith('xcb public locked dependency preload refused: '))
        for guard in p.SAFE_REFUSALS:
            self.assertTrue(p.refusal_message(p.Refused(guard)).endswith(': ' + guard))


if __name__ == '__main__':
    unittest.main()
