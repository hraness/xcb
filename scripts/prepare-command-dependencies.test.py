#!/usr/bin/env python3
"""Hermetic dependency frontend tests: no VM, package manager, network or auth."""
import base64
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import stat
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('prepare', HERE / 'prepare-command-dependencies.py')
app = importlib.util.module_from_spec(spec)
spec.loader.exec_module(app)
SOURCE = HERE.parent


def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    path.chmod(0o600)


class Frontend(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.base = Path(self.temp.name).resolve()
        self.work = self.base / 'workspace'
        self.work.mkdir(mode=0o700)
        write(self.work / 'Cargo.toml', b'[package]\nname="unit"\nversion="0.1.0"\n')
        write(self.work / 'Cargo.lock', b'version = 4\npackage = []\n')

    def tearDown(self):
        self.temp.cleanup()

    def capture(self):
        with app.Capture() as cap:
            return app.manifests(cap, self.work)

    def test_collects_only_enabled_manifests_and_reports_separate_nested_lock(self):
        write(self.work / 'crates/a/Cargo.toml', b'[package]\nname="a"\nversion="0.1.0"')
        write(self.work / 'site/bun.lock', b'not parsed on host')
        write(self.work / 'site/package.json', b'not selected without root bun.lock')
        write(self.work / '.env', b'PRIVATE')
        write(self.work / '.git/config', b'PRIVATE')
        write(self.work / 'node_modules/package.json', b'PRIVATE')
        write(self.work / 'src/main.rs', b'PRIVATE SOURCE')
        files, nested = self.capture()
        self.assertEqual([v['path'] for v in files], ['Cargo.lock', 'Cargo.toml', 'crates/a/Cargo.toml'])
        self.assertEqual(nested, ['site/bun.lock'])
        self.assertNotIn(b'PRIVATE', b''.join(base64.b64decode(row['base64']) for row in files))

    def test_symlink_hardlink_fifo_manifest_refused(self):
        path = self.work / 'Cargo.lock'
        original = path.read_bytes()
        path.unlink()
        source = self.base / 'outside'
        write(source, original)
        os.symlink(source, path)
        with self.assertRaises((ValueError, OSError)):
            self.capture()
        path.unlink()
        os.link(source, path)
        with self.assertRaises(ValueError):
            self.capture()
        path.unlink()
        os.mkfifo(path)
        with self.assertRaises(ValueError):
            self.capture()

    def test_input_replacement_and_new_manifest_invalidate_capture(self):
        with app.Capture() as cap:
            app.manifests(cap, self.work)
            write(self.work / 'next', b'version = 4\npackage = []\n')
            os.replace(self.work / 'next', self.work / 'Cargo.lock')
            with self.assertRaises(ValueError):
                cap.verify()
        with app.Capture() as cap:
            app.manifests(cap, self.work)
            write(self.work / 'new/Cargo.toml', b'added')
            with self.assertRaises(ValueError):
                cap.verify()

    def test_bounds_and_root_pairs_fail_before_guest(self):
        with patch.object(app, 'MAX_FILE', 4):
            with self.assertRaises(ValueError):
                self.capture()
        (self.work / 'Cargo.toml').unlink()
        with self.assertRaises(ValueError):
            self.capture()

    def root(self):
        root = self.base / 'root'
        root.mkdir(mode=0o700)
        for name in ('home', 'lima', 'cache', 'jobs'):
            (root / name).mkdir(mode=0o700)
        write(root / 'xcb-owner.json', b'{"owner":"xcb-command-v1"}\n')
        write(root / 'admission.lock', b'')
        return root

    def test_private_existing_lock_is_required_and_exclusive(self):
        root = self.root()
        with app.Capture() as cap:
            fd = app.private_root(cap, root)
            with app.admission(fd):
                with self.assertRaises(BlockingIOError):
                    with app.admission(fd):
                        pass
            (root / 'admission.lock').unlink()
            with self.assertRaises(FileNotFoundError):
                with app.admission(fd):
                    pass
            self.assertFalse((root / 'admission.lock').exists())
        root.chmod(0o755)
        with app.Capture() as cap:
            with self.assertRaises(ValueError):
                app.private_root(cap, root)

    def module(self):
        path = SOURCE / 'crates/xcb-runtime/src/command/public_cache.py'
        value = importlib.util.spec_from_file_location('public_cache_test', path)
        module = importlib.util.module_from_spec(value)
        value.loader.exec_module(module)
        return module

    def plan(self):
        files, _ = self.capture()
        spec = {'version': 1, 'files': files, 'toolDigests': {name: '1' * 64 for name in app.TOOLS}}
        path = SOURCE / 'crates/xcb-runtime/src/command/public_cache.py'
        plan = {'version': 1, 'platform': 'linux-arm64', 'preloaderSha256': app.sha(path.read_bytes()),
                'toolDigests': spec['toolDigests'], 'inputs': [{'path': row['path'], 'sha256': row['sha256']} for row in files],
                'cargo': True, 'bun': False, 'archives': [], 'repositories': []}
        plan['cacheKey'] = app.sha(app.encode(plan))
        return spec, plan

    def test_plan_is_bound_to_exact_inputs_module_tools_and_key(self):
        spec, plan = self.plan()
        digest = plan['preloaderSha256']
        self.assertEqual(app.validate_plan(plan, spec, SOURCE, digest)['cacheKey'], plan['cacheKey'])
        altered = dict(plan, cacheKey='0' * 64)
        with self.assertRaises(ValueError):
            app.validate_plan(altered, spec, SOURCE, digest)
        with self.assertRaises(ValueError):
            app.validate_plan(plan, dict(spec, files=[]), SOURCE, digest)
        with self.assertRaises(ValueError):
            app.validate_plan(plan, spec, SOURCE, '0' * 64)

    def test_receipt_requires_join_exact_key_closed_shape_and_bounds(self):
        key = '1' * 64
        receipt = {'version': 1, 'cacheKey': key, 'joined': True, 'inventorySha256': '2' * 64, 'bytes': 23, 'entries': 4}
        self.assertEqual(app.validate_receipt(receipt, key), receipt)
        for row in (dict(receipt, joined=False), dict(receipt, cacheKey='3' * 64),
                    dict(receipt, bytes=app.LIMIT + 1), dict(receipt, entries=True),
                    dict(receipt, cachePath='/foreign'), dict(receipt, inventorySha256='bad')):
            with self.subTest(row=row), self.assertRaises(ValueError):
                app.validate_receipt(row, key)

    def test_dry_run_never_prepares_and_does_not_forward_environment(self):
        root = self.root()
        spec, plan = self.plan()
        backend = {'toolDigests': {key: {'sha256': val} for key, val in spec['toolDigests'].items()},
                   'publicCacheSha256': plan['preloaderSha256']}
        calls = []
        def transport(_root, _backend, operation, data, timeout):
            calls.append((operation, app.decode(data), timeout))
            return app.encode({'acknowledged': True} if operation == 'public-cache-ack' else plan)
        with patch.object(app, 'backend', return_value=backend), patch.object(app, 'transport', side_effect=transport):
            result = app.run(root, self.work, False, SOURCE)
        self.assertFalse(result['prepared'])
        self.assertEqual(calls[0], ('public-cache-plan', spec, 60))
        self.assertEqual([row[0] for row in calls], ['public-cache-plan', 'public-cache-ack'])
        self.assertEqual(calls[1][1]['cacheKey'], plan['cacheKey'])
        records = list(root.glob('public-cache-plan-*.json'))
        self.assertEqual(len(records), 1)
        self.assertEqual(app.sha(records[0].read_bytes()), calls[1][1]['hostReceiptSha256'])
        with patch.object(app, 'capture_client', return_value=b'{}') as client:
            app.transport(root, {'limaExecutable': '/trusted/lima'}, 'public-cache-plan', b'{}', 60)
        argv, env, data, timeout = client.call_args.args
        self.assertEqual(argv[-2:], ['/usr/local/lib/xcb-command/guest.py', 'public-cache-plan'])
        self.assertEqual(set(env), {'HOME', 'LIMA_HOME', 'XDG_CACHE_HOME', 'SSH', 'PATH', 'LANG'})
        self.assertEqual(data, b'{}')
        self.assertEqual(timeout, 60)

    def test_changed_input_after_joined_prepare_is_not_accepted(self):
        root = self.root()
        spec, plan = self.plan()
        backend = {'toolDigests': {key: {'sha256': val} for key, val in spec['toolDigests'].items()},
                   'publicCacheSha256': plan['preloaderSha256']}
        calls = []
        def transport(_root, _backend, operation, _data, _timeout):
            calls.append(operation)
            if operation == 'public-cache-plan':
                return app.encode(plan)
            if operation == 'public-cache-ack':
                return app.encode({'acknowledged': True})
            write(self.work / 'Cargo.toml', b'changed while preparing')
            return app.encode({'version': 1, 'cacheKey': plan['cacheKey'], 'joined': True,
                               'inventorySha256': '2' * 64, 'bytes': 0, 'entries': 0})
        with patch.object(app, 'backend', return_value=backend), patch.object(app, 'transport', side_effect=transport):
            with self.assertRaises(ValueError):
                app.run(root, self.work, True, SOURCE)
        self.assertEqual(calls, ['public-cache-plan', 'public-cache-ack', 'public-cache-prepare'])
        self.assertTrue((root / ('public-cache-intent-' + plan['cacheKey'] + '.json')).exists())

    def test_transport_joins_with_clean_child_signal_mask(self):
        code = 'import signal,sys; assert signal.SIGTERM not in signal.pthread_sigmask(signal.SIG_BLOCK, []); print(sys.stdin.buffer.read().decode())'
        result = app.capture_client([sys.executable, '-I', '-c', code], {}, b'joined', 5)
        self.assertEqual(result, b'joined\n')

    def test_transport_timeout_and_overflow_never_return_success(self):
        with self.assertRaises(ValueError):
            app.capture_client([sys.executable, '-I', '-c', 'import time; time.sleep(10)'], {}, b'', 0.05)
        with patch.object(app, 'MAX_OUTPUT', 4), self.assertRaises(ValueError):
            app.capture_client([sys.executable, '-I', '-c', 'print("overflow")'], {}, b'', 5)

    def test_no_signal_can_follow_reap(self):
        events = []
        original_kill, original_wait = os.killpg, app.subprocess.Popen.wait
        def kill(pid, sig):
            events.append(('signal', sig))
            return original_kill(pid, sig)
        def wait(process, *args, **kwargs):
            result = original_wait(process, *args, **kwargs)
            events.append(('reaped', result))
            return result
        with patch.object(os, 'killpg', side_effect=kill), patch.object(app.subprocess.Popen, 'wait', wait):
            app.capture_client([sys.executable, '-I', '-c', 'print("done")'], {}, b'', 5)
        position = next(i for i, row in enumerate(events) if row[0] == 'reaped')
        self.assertTrue(all(kind != 'signal' or value == 0 for kind, value in events[position + 1:]))
        self.assertIn(('signal', signal.SIGKILL), events[:position])

    def test_pending_intent_blocks_resubmit_and_status_cannot_clear(self):
        root = self.root()
        spec, plan = self.plan()
        backend = {'toolDigests': {key: {'sha256': val} for key, val in spec['toolDigests'].items()},
                   'publicCacheSha256': plan['preloaderSha256']}
        with app.Capture() as cap:
            fd = app.private_root(cap, root)
            intent = app.begin_intent(fd, plan['cacheKey'], backend, spec)
            with self.assertRaises(ValueError):
                app.begin_intent(fd, plan['cacheKey'], backend, spec)
            with self.assertRaises(ValueError):
                app.begin_intent(fd, '0' * 64, backend, spec)
        terminal = {'version': 1, 'cacheKey': plan['cacheKey'], 'joined': True, 'prepared': False, 'receipt': None}
        with patch.object(app, 'backend', return_value=backend), patch.object(app, 'transport', return_value=app.encode(terminal)) as transport:
            result = app.reconcile(root, plan['cacheKey'], False, SOURCE)
        self.assertTrue(result['intentRetained'])
        self.assertEqual(transport.call_args.args[2], 'public-cache-status')
        self.assertEqual(app.decode((root / ('public-cache-intent-' + plan['cacheKey'] + '.json')).read_bytes()), intent)

    def test_explicit_recovery_clears_only_joined_exact_terminal_and_retains_record(self):
        root = self.root()
        spec, plan = self.plan()
        backend = {'test': True}
        with app.Capture() as cap:
            fd = app.private_root(cap, root)
            app.begin_intent(fd, plan['cacheKey'], backend, spec)
        pending = root / ('public-cache-intent-' + plan['cacheKey'] + '.json')
        terminal = {'version': 1, 'cacheKey': plan['cacheKey'], 'joined': False, 'prepared': False, 'receipt': None}
        with patch.object(app, 'backend', return_value=backend), patch.object(app, 'transport', return_value=app.encode(terminal)):
            self.assertTrue(app.reconcile(root, plan['cacheKey'], True, SOURCE)['intentRetained'])
        self.assertTrue(pending.exists())
        terminal['joined'] = True
        with patch.object(app, 'backend', return_value=backend), patch.object(app, 'transport', return_value=app.encode(terminal)) as transport:
            self.assertFalse(app.reconcile(root, plan['cacheKey'], True, SOURCE)['intentRetained'])
        self.assertEqual(transport.call_args_list[0].args[2], 'public-cache-recover')
        self.assertFalse(pending.exists())
        records = list(root.glob('public-cache-result-*.json'))
        self.assertEqual(len(records), 1)
        self.assertEqual(app.decode(records[0].read_bytes())['terminal'], terminal)

    def test_uncertain_prepare_retains_intent_and_never_retries(self):
        root = self.root()
        spec, plan = self.plan()
        backend = {'toolDigests': {key: {'sha256': val} for key, val in spec['toolDigests'].items()},
                   'publicCacheSha256': plan['preloaderSha256']}
        calls = []
        def transport(_root, _backend, operation, _data, _timeout):
            calls.append(operation)
            if operation == 'public-cache-plan':
                return app.encode(plan)
            raise ValueError('synthetic lost transport')
        with patch.object(app, 'backend', return_value=backend), patch.object(app, 'transport', side_effect=transport):
            for _ in range(2):
                with self.assertRaises(ValueError):
                    app.run(root, self.work, True, SOURCE)
        self.assertEqual(calls.count('public-cache-prepare'), 1)
        self.assertTrue((root / ('public-cache-intent-' + plan['cacheKey'] + '.json')).exists())

    def test_changed_backend_or_forged_receipt_does_not_release_intent(self):
        root = self.root()
        spec, plan = self.plan()
        original = {'original': True}
        with app.Capture() as cap:
            fd = app.private_root(cap, root)
            app.begin_intent(fd, plan['cacheKey'], original, spec)
        with patch.object(app, 'backend', return_value={'new': True}), patch.object(app, 'transport') as transport:
            with self.assertRaises(ValueError):
                app.reconcile(root, plan['cacheKey'], True, SOURCE)
            transport.assert_not_called()
        forged = {'version': 1, 'cacheKey': '0' * 64, 'joined': True, 'prepared': False, 'receipt': None}
        with patch.object(app, 'backend', return_value=original), patch.object(app, 'transport', return_value=app.encode(forged)):
            with self.assertRaises(ValueError):
                app.reconcile(root, plan['cacheKey'], True, SOURCE)
        self.assertTrue((root / ('public-cache-intent-' + plan['cacheKey'] + '.json')).exists())

    def test_successful_prepare_records_exact_receipt_and_releases_intent(self):
        root = self.root()
        spec, plan = self.plan()
        backend = {'toolDigests': {key: {'sha256': val} for key, val in spec['toolDigests'].items()},
                   'publicCacheSha256': plan['preloaderSha256']}
        receipt = {'version': 1, 'cacheKey': plan['cacheKey'], 'joined': True, 'inventorySha256': '2' * 64, 'bytes': 10, 'entries': 2}
        with patch.object(app, 'backend', return_value=backend), patch.object(app, 'transport', side_effect=[app.encode(plan), app.encode({'acknowledged': True}), app.encode(receipt), app.encode({'acknowledged': True})]) as transport:
            result = app.run(root, self.work, True, SOURCE)
        self.assertEqual(app.decode(transport.call_args_list[2].args[3]), {'version': 1, 'expectedCacheKey': plan['cacheKey'], 'spec': spec})
        self.assertTrue(result['prepared'])
        self.assertEqual(result['receipt'], receipt)
        self.assertFalse(list(root.glob('public-cache-intent-*.json')))
        self.assertEqual(len(list(root.glob('public-cache-result-*.json'))), 1)


    def test_term_during_transport_joins_child_before_returning(self):
        previous = signal.signal(signal.SIGTERM, app.interrupted)
        original_read, original_spawn = os.read, app.subprocess.Popen
        fired, children = [], []
        def spawn(*args, **kwargs):
            child = original_spawn(*args, **kwargs)
            children.append(child.pid)
            return child
        def read(fd, size):
            value = original_read(fd, size)
            if value == b'ready\n' and not fired:
                fired.append(True)
                os.kill(os.getpid(), signal.SIGTERM)
            return value
        try:
            with patch.object(os, 'read', side_effect=read), patch.object(app.subprocess, 'Popen', side_effect=spawn):
                with self.assertRaises(InterruptedError):
                    app.capture_client([sys.executable, '-I', '-c', 'import time; print("ready",flush=True); time.sleep(10)'], {}, b'', 5)
        finally:
            signal.signal(signal.SIGTERM, previous)
        self.assertEqual(fired, [True])
        self.assertEqual(len(children), 1)
        with self.assertRaises(ProcessLookupError):
            os.killpg(children[0], 0)


    def test_ack_failure_is_cleanup_only_after_durable_receipt(self):
        root = self.root()
        spec, plan = self.plan()
        backend = {'toolDigests': {key: {'sha256': val} for key, val in spec['toolDigests'].items()},
                   'publicCacheSha256': plan['preloaderSha256']}
        receipt = {'version': 1, 'cacheKey': plan['cacheKey'], 'joined': True, 'inventorySha256': '2' * 64, 'bytes': 10, 'entries': 2}
        def transport(_root, _backend, operation, data, _timeout):
            if operation == 'public-cache-plan':
                return app.encode(plan)
            if operation == 'public-cache-prepare':
                return app.encode(receipt)
            self.assertEqual(operation, 'public-cache-ack')
            request = app.decode(data)
            records = list(root.glob('public-cache-plan-*.json')) + list(root.glob('public-cache-result-*.json'))
            self.assertTrue(any(app.sha(path.read_bytes()) == request['hostReceiptSha256'] for path in records))
            raise ValueError('synthetic cleanup failure')
        with patch.object(app, 'backend', return_value=backend), patch.object(app, 'transport', side_effect=transport):
            result = app.run(root, self.work, True, SOURCE)
        self.assertTrue(result['prepared'])
        self.assertTrue(result['cleanupPending'])
        self.assertEqual(result['receipt'], receipt)
        self.assertFalse(list(root.glob('public-cache-intent-*.json')))


    def test_interrupt_during_plan_ack_never_begins_prepare(self):
        root = self.root()
        spec, plan = self.plan()
        backend = {'toolDigests': {key: {'sha256': val} for key, val in spec['toolDigests'].items()},
                   'publicCacheSha256': plan['preloaderSha256']}
        def transport(_root, _backend, operation, _data, _timeout):
            if operation == 'public-cache-plan':
                return app.encode(plan)
            self.assertEqual(operation, 'public-cache-ack')
            raise InterruptedError('synthetic interruption')
        with patch.object(app, 'backend', return_value=backend), patch.object(app, 'transport', side_effect=transport) as transport_mock:
            with self.assertRaises(InterruptedError):
                app.run(root, self.work, True, SOURCE)
        self.assertEqual(transport_mock.call_count, 2)
        self.assertFalse(list(root.glob('public-cache-intent-*.json')))
        with patch.object(app, 'transport', side_effect=InterruptedError('synthetic')):
            self.assertTrue(app.acknowledge(root, backend, plan['cacheKey'], '1' * 64, terminal=True))


if __name__ == '__main__':
    unittest.main()
