#!/usr/bin/python3
"""No host Git. Actual Git cases run only in an unprivileged Linux guest."""
import base64
import hashlib
import importlib.util
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zlib

spec = importlib.util.spec_from_file_location('git_projection', Path(__file__).with_name('git_projection.py'))
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)


def object_file(kind, data):
    raw = kind.encode() + b' ' + str(len(data)).encode() + b'\0' + data
    oid = hashlib.sha1(raw).hexdigest()
    return oid, ('objects/' + oid[:2] + '/' + oid[2:], zlib.compress(raw))


def index_file(entries, version=2, extension=b''):
    raw = b'DIRC' + struct.pack('>II', version, len(entries))
    for name, oid, mode, flags in entries:
        name = name.encode()
        values = [0] * 10
        values[6] = mode
        entry = struct.pack('>10I', *values) + bytes.fromhex(oid) + struct.pack('>H', flags | min(len(name), 0xfff)) + name + b'\0'
        raw += entry + b'\0' * ((-len(entry)) % 8)
    raw += extension
    return raw + hashlib.sha1(raw).digest()


def dto(files, head=None):
    return {'version': 1, 'headObjectId': head, 'files': [
        {'path': path, 'base64': base64.b64encode(data).decode(), 'sha256': p.sha(data)}
        for path, data in sorted(files.items())]}


def fixture(unborn=False):
    files = {'HEAD': b'ref: refs/heads/source-private-branch\n'}
    def obj(kind, data):
        oid, row = object_file(kind, data)
        files[row[0]] = row[1]
        return oid
    base = obj('blob', b'base\n')
    staged = obj('blob', b'staged\n')
    secret = obj('blob', b'SECRET_EXCLUDED_BYTES\n')
    old = obj('blob', b'PRIVATE_DELETED_HISTORY_BYTES\n')
    old_tree = obj('tree', b'100644 old-secret\0' + bytes.fromhex(old))
    identity = b'author PRIVATE_SOURCE_AUTHOR <private@example.invalid> 1000000000 +0000\ncommitter PRIVATE_SOURCE_AUTHOR <private@example.invalid> 1000000000 +0000\n'
    old_commit = obj('commit', b'tree ' + old_tree.encode() + b'\n' + identity + b'\nPRIVATE_OLD_MESSAGE\n')
    tree = obj('tree', b'100644 .env\0' + bytes.fromhex(secret) + b'100644 input.txt\0' + bytes.fromhex(base))
    head = obj('commit', b'tree ' + tree.encode() + b'\nparent ' + old_commit.encode() + b'\n' + identity + b'\nPRIVATE_HEAD_MESSAGE\n')
    files['index'] = index_file([('.env', secret, 0o100644, 0), ('input.txt', staged, 0o100644, 0)])
    if not unborn:
        files['refs/heads/source-private-branch'] = head.encode() + b'\n'
    return dto(files, None if unborn else head)


class ProjectionUnitTests(unittest.TestCase):
    def test_closed_snapshot_and_exact_raw_paths(self):
        good = dto({'HEAD': b'ref: refs/heads/main\n'})
        self.assertEqual(p.validate_snapshot(good)[0], None)
        for branch in ('_branch', '-branch', 'feature/topic'):
            self.assertTrue(p.raw_path('refs/heads/' + branch))
        for branch in ('.hidden', 'one.lock/two', 'one./two', 'one/.two'):
            self.assertFalse(p.raw_path('refs/heads/' + branch))
        for name in ('config', 'hooks/pre-commit', 'objects/info/alternates', 'commondir', '../HEAD', 'refs/remotes/origin/main'):
            with self.assertRaises(ValueError, msg=name):
                p.validate_snapshot(dto({'HEAD': b'ref: refs/heads/main\n', name: b'forbidden'}))
        with self.assertRaises(ValueError):
            p.validate_snapshot({**good, 'extra': True})
        with self.assertRaises(ValueError):
            p.decode(b'{"version":1,"version":1}')

    def test_snapshot_digest_count_and_identity_fail_closed(self):
        good = dto({'HEAD': b'ref: refs/heads/main\n'})
        good['files'][0]['sha256'] = 'f' * 64
        with self.assertRaises(ValueError):
            p.validate_snapshot(good)
        duplicate = dto({'HEAD': b'ref: refs/heads/main\n'})
        duplicate['files'].append(duplicate['files'][0].copy())
        with self.assertRaises(ValueError):
            p.validate_snapshot(duplicate)
        with self.assertRaises(ValueError):
            p.validate_snapshot(dto({'HEAD': b'a' * 40 + b'\n'}, 'b' * 40))
        with patch.object(p, 'MAX_ENTRIES', 0), self.assertRaises(ValueError):
            p.validate_snapshot(dto({'HEAD': b'ref: refs/heads/main\n'}))
        with patch.object(p, 'MAX_RAW', 1), self.assertRaises(ValueError):
            p.validate_snapshot(dto({'HEAD': b'ref: refs/heads/main\n'}))

    def test_index_checksum_and_stage_flags_modes_extensions_are_explicit(self):
        entry = ('input', '1' * 40, 0o100644, 0)
        for version in (2, 3):
            self.assertEqual(p.validate_index(index_file([entry], version)), [('100644', '1' * 40, 'input')])
        for mode, flags in ((0o120000, 0), (0o160000, 0), (0o100644, 0x1000), (0o100644, 0x4000), (0o100644, 0x8000)):
            with self.assertRaises(ValueError):
                p.validate_index(index_file([('input', '1' * 40, mode, flags)]))
        for extension in (b'link\0\0\0\0', b'sdir\0\0\0\0', b'FSMN\0\0\0\0'):
            with self.assertRaises(ValueError):
                p.validate_index(index_file([entry], extension=extension))
        corrupt = bytearray(index_file([entry]))
        corrupt[20] ^= 1
        with self.assertRaises(ValueError):
            p.validate_index(bytes(corrupt))
        with self.assertRaises(ValueError):
            p.validate_index(index_file([entry], 4))

    def test_index_rejects_unordered_and_file_ancestor_paths(self):
        for names in (('z', 'a'), ('a', 'a/child')):
            entries = [(name, '1' * 40, 0o100644, 0) for name in names]
            with self.assertRaises(ValueError):
                p.validate_index(index_file(entries))

    def test_filters_match_secret_and_dependency_conventions(self):
        for path in ('a/.git/config', '.env', 'a/.env.local', '.codex/auth.json', 'node_modules/x', '.xcb-command-1', 'target/debug/x'):
            self.assertTrue(p.excluded(path), path)
        for path in ('.env.example', 'a/.env.sample', '.env.template', 'source/config.rs'):
            self.assertFalse(p.excluded(path), path)
        raw = b'100644 blob ' + b'1' * 40 + b'\t.env\0' + b'100755 blob ' + b'2' * 40 + b'\tscript\0'
        self.assertEqual(p.tree_entries(raw), [('100755', '2' * 40, 'script')])
        for mode, kind in (('120000', 'blob'), ('160000', 'commit')):
            with self.assertRaises(ValueError):
                p.tree_entries((mode + ' ' + kind + ' ' + '1' * 40 + '\tunsupported\0').encode())

    def test_bwrap_mounts_only_output_and_runtime_with_clean_environment(self):
        argv = p.sandbox_argv(Path('/private/projection-output'))
        self.assertEqual(argv[0], '/usr/bin/bwrap')
        self.assertIn('--unshare-all', argv)
        self.assertIn('--disable-userns', argv)
        self.assertIn('--clearenv', argv)
        self.assertEqual(argv[-4:], ['--', '/usr/bin/python3', '-I', p.MODULE])
        self.assertEqual(argv.count('--bind'), 1)
        where = argv.index('--bind')
        self.assertEqual(argv[where + 1:where + 3], ['/private/projection-output', '/output'])
        self.assertNotIn('/var/lib/xcb-command', argv)
        self.assertNotIn('/home', argv)
        self.assertEqual(p.ENV['GIT_OPTIONAL_LOCKS'], '0')
        self.assertEqual(p.ENV['GIT_CONFIG_GLOBAL'], '/dev/null')

    def test_root_execution_is_rejected_before_any_git_call(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(p.os, 'geteuid', return_value=0), patch.object(p.Git, 'run') as git:
            with self.assertRaises(ValueError):
                p.materialize(dto({'HEAD': b'ref: refs/heads/main\n'}), Path(temporary))
            git.assert_not_called()

    def test_output_validator_rejects_config_symlink_hardlink_and_oversize(self):
        for invalid in ('config', 'symlink', 'hardlink', 'oversize'):
            with tempfile.TemporaryDirectory() as temporary:
                output = Path(temporary)
                git = output / '.git'
                p.init(git)
                (git / 'index').write_bytes(index_file([]))
                self.assertRegex(p.validate_output(output), r'^[a-f0-9]{64}$')
                if invalid == 'config':
                    (git / 'config').write_bytes(b'[remote "unsafe"]\nurl=x\n')
                elif invalid == 'symlink':
                    (git / 'index').unlink()
                    (git / 'index').symlink_to(git / 'HEAD')
                elif invalid == 'hardlink':
                    os.link(git / 'index', git / p.BRANCH)
                else:
                    with open(git / 'index', 'wb') as file:
                        file.truncate(3 * 1024 * 1024 + 1)
                with self.assertRaises(ValueError, msg=invalid):
                    p.validate_output(output)


@unittest.skipUnless(sys.platform == 'linux' and os.geteuid() != 0, 'actual Git only inside unprivileged Linux guest')
class ProjectionGuestTests(unittest.TestCase):
    def git(self, work, *args):
        return subprocess.check_output(['/usr/bin/git', '-C', str(work), *args], env=p.ENV, stderr=subprocess.PIPE, timeout=5)

    def test_filtered_head_index_preserve_staged_unstaged_and_strip_history(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            result = p.materialize(fixture(), output)
            (output / 'input.txt').write_bytes(b'working\n')
            self.assertEqual(self.git(output, 'status', '--porcelain=v1'), b'MM input.txt\n')
            self.assertIn(b'-base\n+staged\n', self.git(output, 'diff', '--cached', '--no-ext-diff', '--', 'input.txt'))
            self.assertIn(b'-staged\n+working\n', self.git(output, 'diff', '--no-ext-diff', '--', 'input.txt'))
            self.assertEqual(self.git(output, 'rev-list', '--count', 'HEAD'), b'1\n')
            commit = self.git(output, 'cat-file', 'commit', 'HEAD')
            self.assertIn(p.MESSAGE, commit)
            self.assertNotIn(b'parent ', commit)
            self.assertEqual((result['headFiles'], result['indexFiles']), (1, 1))
            for path in (output / '.git/objects').glob('*/*'):
                data = zlib.decompress(path.read_bytes())
                for marker in (b'SECRET_EXCLUDED_BYTES', b'PRIVATE_', b'private@example.invalid'):
                    self.assertNotIn(marker, data)
            self.assertEqual(p.validate_output(output), result['projectionSha256'])
            for path in ('config', 'hooks', 'logs', 'packed-refs'):
                self.assertFalse((output / '.git' / path).exists())

    def test_unborn_head_preserves_staged_addition_without_fabricating_commit(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            result = p.materialize(fixture(unborn=True), output)
            (output / 'input.txt').write_bytes(b'staged\n')
            self.assertEqual(self.git(output, 'status', '--porcelain=v1'), b'A  input.txt\n')
            self.assertEqual(result['headFiles'], 0)
            self.assertFalse((output / '.git' / p.BRANCH).exists())

    def test_corrupt_object_fails_full_validation(self):
        snapshot = fixture()
        for entry in snapshot['files']:
            if entry['path'].startswith('objects/'):
                data = zlib.compress(b'blob 7\0corrupt')
                entry['base64'] = base64.b64encode(data).decode()
                entry['sha256'] = p.sha(data)
                break
        with tempfile.TemporaryDirectory() as temporary, self.assertRaises(ValueError):
            p.materialize(snapshot, Path(temporary))


if __name__ == '__main__':
    unittest.main()
