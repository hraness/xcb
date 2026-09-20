#!/usr/bin/env python3
"""Hermetic parser/export tests; no VM, provider, credentials, or root required."""
import base64
import importlib.util
import os
import pathlib
import sys
import tempfile
import unittest
from unittest.mock import patch, Mock
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('guest', pathlib.Path(__file__).with_name('guest.py'))
guest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guest)

class GuestTests(unittest.TestCase):
    def seed(self, files=None, directories=None):
        return {'version':1, 'workspaceId':'a'*64, 'files':files or [], 'directories':directories or []}
    def entry(self, name, data=b'initial'):
        return {'path':name, 'base64':base64.b64encode(data).decode(), 'sha256':guest.digest(data), 'executable':False}
    def test_closed_request_and_offline_network(self):
        good={'argv':['sh','-c','printf hello'], 'cwd':'.', 'timeoutMs':1000, 'network':'none'}
        guest.validate_request(good)
        for patch in ({'network':'all'}, {'timeoutMs':0}, {'timeoutMs':True}, {'env':{}}, {'cwd':'../x'}, {'argv':['x\0y']}):
            with self.subTest(patch=patch), self.assertRaises(ValueError):guest.validate_request(dict(good,**patch))
    def test_duplicate_json_and_snapshot_paths(self):
        with self.assertRaises(ValueError):guest.decode(b'{"version":1,"version":1}')
        with self.assertRaises(ValueError):guest.validate_snapshot(self.seed([self.entry('a'),self.entry('a')]),'a'*64)
        with self.assertRaises(ValueError):guest.validate_snapshot(self.seed([self.entry('a')],['a/child']),'a'*64)
    def test_snapshot_digest_and_size_and_mode(self):
        for patch in ({'sha256':'b'*64}, {'executable':1}, {'base64':'??'}):
            with self.subTest(patch=patch),self.assertRaises((ValueError,TypeError)):
                guest.validate_snapshot(self.seed([dict(self.entry('a'),**patch)]),'a'*64)
        with self.assertRaises(ValueError):guest.validate_snapshot(self.seed([self.entry('a',b'x'*(2*1024*1024+1))]),'a'*64)
    def test_exclusions_apply_to_input_at_any_depth(self):
        for name in ('.git/HEAD','a/node_modules/x','.env','a/.env.local','x/.ssh/key','a/target/z','a/.xcb-stage'):
            with self.subTest(name=name),self.assertRaises(ValueError):guest.validate_snapshot(self.seed([self.entry(name)]),'a'*64)
        guest.validate_snapshot(self.seed([self.entry('.env.example')]),'a'*64)
    def test_changes_preserve_edits_deletion_and_ignore_generated(self):
        with tempfile.TemporaryDirectory() as temporary:
            job=pathlib.Path(temporary);work=job/'work';work.mkdir()
            (job/'seed.json').write_bytes(guest.encode(self.seed([self.entry('deleted'),self.entry('edited')])))
            (work/'edited').write_bytes(b'new');(work/'target').mkdir();(work/'target'/'big').write_bytes(b'x'*(2*1024*1024+1))
            (work/'.env').write_text('synthetic-secret')
            result=guest.decode(guest.changes(job,'a'*64))
            self.assertEqual([c['path'] for c in result['changes']],['deleted','edited'])
            self.assertIsNone(result['changes'][0]['base64'])
    def test_nonregular_hardlink_and_symlink_fail_without_blocking(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=pathlib.Path(temporary);f=root/'file';f.write_bytes(b'test')
            os.mkfifo(root/'fifo');os.symlink(f,root/'symlink');os.link(f,root/'hardlink')
            for p in (f,root/'fifo',root/'symlink'):
                with self.subTest(path=p),self.assertRaises((ValueError,OSError)):guest.read_regular(p,100)
    def test_unicode_request_uses_utf8_canonical_bytes(self):
        self.assertEqual(guest.encode({'value':'é'}),b'{"value":"\xc3\xa9"}')
    def test_populated_requires_a_real_closed_state(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=pathlib.Path(temporary)
            for text,want in [('populated 1\nfrozen 0\n',True),('populated 0\n',False)]:
                (root/'cgroup.events').write_text(text);self.assertEqual(guest.populated(root),want)
            (root/'cgroup.events').write_text('populated maybe\n')
            with self.assertRaises(ValueError):guest.populated(root)

class CacheGuestTests(unittest.TestCase):
 def entry(self,path,data=b'{}'):
  return {'path':path,'base64':base64.b64encode(data).decode(),'sha256':guest.digest(data),'executable':False}
 def test_snapshot_manifest_selector_matches_root_ecosystems_and_ignores_nested_locks(self):
  names=['Cargo.toml','Cargo.lock','crates/a/Cargo.toml','nested/Cargo.lock','package.json','bun.lock','site/package.json','site/bun.lock','src/lib.rs']
  tools={name:{'sha256':name} for name in ('bun','cargo','git','node','python','rustc')}
  spec=guest.cache_spec_from_snapshot({'files':[self.entry(path) for path in names]},tools)
  self.assertEqual([row['path'] for row in spec['files']],['Cargo.lock','Cargo.toml','bun.lock','crates/a/Cargo.toml','package.json','site/package.json'])
  self.assertEqual(set(spec['files'][0]),{'path','sha256','base64'})
  changed=[self.entry(path,b'changed' if path=='Cargo.toml' else b'{}') for path in names]
  self.assertNotEqual(guest.cache_spec_from_snapshot({'files':changed},tools),spec)
 def test_no_lockfile_has_no_cache_plan(self):
  self.assertIsNone(guest.cache_spec_from_snapshot({'files':[self.entry('Cargo.toml')]},{}))
 def test_no_joined_receipt_cannot_unblock_pending_prepare(self):
  with tempfile.TemporaryDirectory() as root:
   root=pathlib.Path(root);job=root/'preloads/cache_test';job.mkdir(parents=True)
   (job/'custody.json').write_bytes(guest.encode({'operation':'public-cache-prepare'}))
   with patch.object(guest,'ROOT',root),self.assertRaises(ValueError):guest.cache_pending()
 def test_receipt_must_match_exact_custody(self):
  with tempfile.TemporaryDirectory() as root:
   job=pathlib.Path(root);(job/'custody.json').write_bytes(guest.encode({'version':1,'key':'original'}))
   value={'version':1,'custody':{'version':1,'key':'other'},'joined':True,'plan':None,'receipt':None,'error':None}
   (job/'result.json').write_bytes(guest.encode(value))
   with self.assertRaises(ValueError):guest.cache_outcome(job)
 def test_cache_mount_keeps_cargo_home_private_and_writable(self):
  with tempfile.TemporaryDirectory() as root:
   root=pathlib.Path(root);(root/'cargo').mkdir();(root/'cargo/config.toml').write_text('fixed');(root/'bun').mkdir()
   argv=guest.sandbox_argv(pathlib.Path('/work-source'),{'cwd':'.','argv':['true']},cache=root)
   self.assertIn('/home/xcb/.cargo/config.toml',argv)
   self.assertNotIn('CARGO_HOME',argv)
   self.assertIn('BUN_INSTALL_CACHE_DIR',argv)
 def test_unobserved_submission_never_recovers_from_service_absence(self):
  with tempfile.TemporaryDirectory() as root:
   job=pathlib.Path(root)/'cache_test';job.mkdir()
   (job/'custody.json').write_bytes(guest.encode({'version':1,'expectedCacheKey':'a'*64}))
   with patch.object(guest.subprocess,'run') as command,self.assertRaises(FileNotFoundError):guest.cache_recover_attempt(job)
   command.assert_not_called()
   self.assertFalse((job/'recovered.json').exists())
 def test_started_service_binding_precedes_any_recovery_observation(self):
  with tempfile.TemporaryDirectory() as root:
   job=pathlib.Path(root)/'cache_test';job.mkdir()
   (job/'custody.json').write_bytes(guest.encode({'version':1,'key':'original'}))
   (job/'service-started.json').write_bytes(guest.encode({'version':1,'custody':{'version':1,'key':'other'},'control':{}}))
   with patch.object(guest.subprocess,'run') as command,self.assertRaises(ValueError):guest.cache_recover_attempt(job)
   command.assert_not_called()
 def test_observed_dead_service_recovers_without_publishing_and_is_idempotent(self):
  with tempfile.TemporaryDirectory() as root:
   job=pathlib.Path(root)/'cache_test';job.mkdir()
   custody={'version':1,'expectedCacheKey':'a'*64}
   control={'path':'/sys/fs/cgroup/system.slice/xcb-preload-cache_test.service/worker','dev':1,'ino':2}
   (job/'custody.json').write_bytes(guest.encode(custody));(job/'cgroup.json').write_bytes(guest.encode(control))
   (job/'service-started.json').write_bytes(guest.encode({'version':1,'custody':custody,'control':control}))
   observed=Mock(returncode=0,stdout=b'MainPID=0\nControlGroup=\nActiveState=inactive\nSubState=dead\nLoadState=not-found\n')
   with patch.object(guest.subprocess,'run',return_value=observed) as command:
    first=guest.cache_recover_attempt(job);second=guest.cache_recover_attempt(job)
   self.assertEqual(first,second);self.assertTrue(first['joined']);self.assertIsNone(first['receipt']);self.assertIsNotNone(first['error'])
   self.assertEqual(command.call_count,1)
 def test_changed_cache_image_never_reaches_mount(self):
  with patch.object(guest,'cache_image_identity',return_value={'dev':1,'ino':2,'size':1024**3}),patch.object(guest.subprocess,'run') as command:
   with self.assertRaises(ValueError):guest.ensure_cache_mount(pathlib.Path('/unused'),{'image':{'dev':1,'ino':3,'size':1024**3}})
   command.assert_not_called()
 def test_phase_success_never_accepts_unjoined_or_timeout(self):
  good={'joined':True,'exitCode':0,'error':None,'timedOut':False,'cancelled':False,'truncated':False}
  self.assertTrue(guest.phase_success(good))
  for patch in ({'joined':False},{'exitCode':1},{'timedOut':True},{'cancelled':True},{'truncated':True},{'error':'failed'}):
   self.assertFalse(guest.phase_success(dict(good,**patch)))

if __name__=='__main__':unittest.main()
