#!/usr/bin/env python3
"""Synthetic renewal tests: no XCB, provider, Cargo, launchctl, or install calls."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import plistlib
import signal
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("renewal", Path(__file__).with_name("application-renewal.py"))
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)


class RenewalTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="xcb-renewal-synthetic-")
        self.root = Path(self.temp.name).resolve()
        self.directory = r.private_directory(self.root / "renewal")
        r.private_directory(self.directory / "attempts")
        self.state = r.private_directory(self.root / "state")
        r.private_directory(self.state / "accounts")
        account = r.private_directory(self.state / "accounts/a_synthetic")
        self.generation = account / "application-generation.json"
        r.write_once(self.generation, r.encoded({"version": 1, "account": "a_synthetic", "generation": "1" * 64}))
        self.binary = self.root / "xcb"
        r.write_once(self.binary, b"synthetic-not-an-executable")
        self.runtime_bin = self.root / "runtime-bin"
        self.runtime_bin.mkdir(mode=0o700)
        self.runtimes = {}
        for name in ("node", "bun"):
            path = self.runtime_bin / name
            r.write_once(path, b"synthetic-runtime-not-invoked")
            path.chmod(0o700)
            self.runtimes[name] = str(path)
        self.binding = {"account": "a_synthetic", "model": "claude/sonnet/low", "generation": "1" * 64,
                        "source": str(self.root), "state": str(self.state), "xcb": str(self.binary),
                        "provider_executable": "/synthetic/claude", "scheduler": str(self.binary),
                        "scheduler_target": str(self.binary), "python": "/usr/bin/python3", "script": "/synthetic/application-renewal.py",
                        "cargo": "/synthetic/cargo", "home": str(self.root), **self.runtimes,
                        "environment": r.environment(str(self.root), str(self.binary), self.runtimes["node"], self.runtimes["bun"]), "ambient": {},
                        "files": {path: r.file_hash(path) for path in (str(self.binary), *self.runtimes.values())}, "context": {"synthetic": True},
                        "source_sha256": "2" * 64}

    def tearDown(self):
        self.temp.cleanup()

    def test_json_rejects_duplicate_nonfinite(self):
        for raw in (b'{"a":1,"a":2}', b'{"a":NaN}'):
            with self.assertRaises(ValueError):
                r.decode(raw)

    def test_private_file_rejects_symlink_hardlink_fifo_and_broad_mode(self):
        original = self.directory / "file"
        r.write_once(original, b"synthetic")
        self.assertEqual(r.read(original), b"synthetic")
        alias = self.directory / "alias"
        alias.symlink_to(original)
        with self.assertRaises(ValueError):
            r.read(alias)
        alias.unlink()
        os.link(original, alias)
        with self.assertRaises(ValueError):
            r.read(original)
        alias.unlink()
        original.chmod(0o644)
        with self.assertRaises(ValueError):
            r.read(original)
        fifo = self.directory / "fifo"
        os.mkfifo(fifo, 0o600)
        with self.assertRaises(ValueError):
            r.read(fifo)

    def test_owner_excludes_second_run_and_keeps_lock_inode(self):
        with r.owner(self.directory):
            inode = (self.directory / "owner.lock").stat().st_ino
            with self.assertRaises(ValueError):
                with r.owner(self.directory):
                    self.fail("second owner admitted")
        with r.owner(self.directory):
            self.assertEqual(inode, (self.directory / "owner.lock").stat().st_ino)

    def test_generation_change_and_source_change_fail_closed(self):
        with patch.object(r, "source_digest", return_value="2" * 64):
            r.verify(self.binding)
        with patch.object(r, "source_digest", return_value="3" * 64):
            with self.assertRaises(ValueError):
                r.verify(self.binding)
        self.generation.write_bytes(r.encoded({"version": 1, "account": "a_synthetic", "generation": "4" * 64}))
        with self.assertRaises(ValueError):
            r.verify(self.binding, False)

    def test_executable_replacement_and_scheduler_repoint_fail_closed(self):
        self.binary.write_bytes(b"replacement")
        with self.assertRaises(ValueError):
            r.verify(self.binding, False)
        self.binding["scheduler_target"] = "/other/scheduler"
        with self.assertRaises(ValueError):
            r.verify(self.binding, False)

    def test_collect_argv_has_fresh_collection_fixed_native_scheduler(self):
        evidence = self.directory / "attempts/new/prerequisites"
        argv = r.collect_argv(self.binding, evidence)
        self.assertEqual(argv[:5], [str(self.binary), "--mode=exclusive", "--lane=mac-native", "--label=xcb-application-renewal-prerequisites", "--"])
        self.assertIn("collect", argv)
        self.assertNotIn("bundle", argv)
        self.assertNotIn("--capture", argv)
        self.assertNotIn("--provider-boundary", argv)
        self.assertEqual(argv[argv.index("--output") + 1], str(evidence))
        self.assertEqual(argv[argv.index("--account") + 1], "a_synthetic")

    def test_launchd_has_no_immediate_run_and_no_shell(self):
        label, path, data = r.job(self.binding, self.directory)
        job = plistlib.loads(data)
        self.assertTrue(label.startswith("dev.xcb.application-renewal."))
        self.assertEqual(path.name, label + ".plist")
        self.assertEqual(job["ProgramArguments"], ["/usr/bin/python3", "-I", "/synthetic/application-renewal.py", "run", "--directory", str(self.directory)])
        self.assertFalse(job["RunAtLoad"])
        self.assertEqual(job["StartInterval"], 3600)
        self.assertNotIn("KeepAlive", job)
        self.assertNotIn("StandardOutPath", job)
        self.assertNotIn("ProcessType", job)
        self.assertNotIn("LowPriorityIO", job)
        self.assertEqual(set(job), {"Label", "ProgramArguments", "WorkingDirectory",
                                  "EnvironmentVariables", "StartInterval", "RunAtLoad", "ExitTimeOut"})

    def test_environment_omits_ambient_credentials_and_rust_overrides(self):
        env = r.environment(str(self.root), "/synthetic/scheduler/host-run", "/synthetic/node-bin/node", "/synthetic/bun-bin/bun")
        self.assertEqual(set(env), {"HOME", "PATH", "LANG", "LC_ALL", "BUN_CONFIG_NO_ENV_FILE", "CARGO_HOME", "CARGO_INCREMENTAL"})
        self.assertEqual(env["CARGO_INCREMENTAL"], "0")
        self.assertTrue(env["PATH"].startswith("/synthetic/bun-bin:/synthetic/node-bin:/synthetic/scheduler:"))

    def test_controlled_runtime_resolution_and_replacement_are_checked(self):
        r.verify_test_runtimes(self.binding)
        self.binding["environment"]["PATH"] = "/unavailable"
        with self.assertRaises(ValueError):
            r.verify(self.binding, False)
        self.binding["environment"] = r.environment(str(self.root), str(self.binary), self.runtimes["node"], self.runtimes["bun"])
        Path(self.runtimes["node"]).write_bytes(b"changed-node")
        with self.assertRaises(ValueError):
            r.verify(self.binding, False)

    def test_shadowed_runtime_is_refused(self):
        other = self.root / "other"
        other.mkdir()
        shadow = other / "node"
        shadow.write_bytes(b"unrelated-runtime")
        shadow.chmod(0o700)
        self.binding["environment"]["PATH"] = str(other) + ":" + self.binding["environment"]["PATH"]
        with self.assertRaises(ValueError):
            r.verify_test_runtimes(self.binding)

    def test_unknown_binding_is_rejected_before_commands(self):
        r.write_once(self.directory / "binding.json", r.encoded({"schema": r.SCHEMA, "arbitrary_command": "no"}))
        with patch.object(r, "command") as child:
            with self.assertRaises(ValueError):
                r.load(self.directory)
            child.assert_not_called()

    def invoke(self, qualification=None, phase=None, verify=None):
        stack = contextlib.ExitStack()
        stack.enter_context(patch.object(r, "load", return_value=self.binding))
        stack.enter_context(patch.object(r, "verify", side_effect=verify))
        stack.enter_context(patch.object(r, "require_conditional_qualifier"))
        stack.enter_context(patch.object(r, "capabilities", side_effect=qualification))
        stack.enter_context(patch.object(r, "inspect", return_value=self.binding["context"]))
        mock_phase = stack.enter_context(patch.object(r, "phase", side_effect=phase))
        stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
        return stack, mock_phase

    def test_current_receipt_and_busy_account_launch_nothing(self):
        for value in ({"expiresAt": r.now_ms() + r.DAY_MS}, r.AccountBusy("busy")):
            stack, child = self.invoke(qualification=[value])
            with stack:
                r.run(self.directory)
            child.assert_not_called()
        self.assertFalse((self.directory / "pending.json").exists())

    def test_existing_intent_prevents_blind_provider_retry(self):
        r.write_once(self.directory / "pending.json", b'{"attempt":"old"}')
        stack, child = self.invoke()
        with stack:
            with self.assertRaises(ValueError):
                r.run(self.directory)
        child.assert_not_called()
        self.assertEqual(r.read(self.directory / "pending.json"), b'{"attempt":"old"}')

    def test_source_drift_blocks_before_any_capability_or_provider_use(self):
        stack, child = self.invoke(verify=ValueError("source drift"))
        with stack:
            with self.assertRaises(ValueError):
                r.run(self.directory)
        child.assert_not_called()
        self.assertFalse((self.directory / "pending.json").exists())

    def test_failed_refresh_retains_intent_and_stops(self):
        stack, child = self.invoke(qualification=[None], phase=ValueError("unproven"))
        with stack:
            with self.assertRaises(ValueError):
                r.run(self.directory)
        self.assertEqual(child.call_count, 1)
        self.assertEqual(child.call_args.args[2], "refresh")
        self.assertTrue((self.directory / "pending.json").exists())

    def test_failed_phase_records_bounded_evidence(self):
        attempt = r.private_directory(self.directory / "attempts/failed")
        with patch.object(r, "command", return_value=(1, b"synthetic failure")):
            with self.assertRaises(ValueError):
                r.phase(self.binding, attempt, "qualify", ["/synthetic/xcb"], 10)
        self.assertEqual(r.read(attempt / "qualify.output"), b"synthetic failure")
        self.assertEqual(r.decode(r.read(attempt / "qualify.exit.json"))["exit_code"], 1)
        self.assertTrue((attempt / "qualify.intent.json").exists())

    def test_evidence_budget_preserves_all_attempts(self):
        for index in range(r.MAX_ATTEMPTS):
            r.private_directory(self.directory / "attempts" / str(index))
        with self.assertRaises(ValueError):
            r.evidence_budget(self.directory)
        self.assertEqual(len(list((self.directory / "attempts").iterdir())), r.MAX_ATTEMPTS)

    def receipt(self, observed, expires):
        location = self.state
        for name in ("qualification", "application-v1", "a_synthetic"):
            location = r.private_directory(location / name)
        raw = r.encoded({"binding": {"credential_generation": "1" * 64,
            "models": ["claude/sonnet/low"]}, "observed_at_ms": observed, "expires_at_ms": expires})
        r.write_once(location / "receipt.json", raw)
        self.receipt_digest = r.sha(raw)

    def test_success_requires_new_receipt_and_preserves_native_expiry(self):
        stamp = r.now_ms()
        expires = stamp + r.DAY_MS
        self.receipt(stamp, expires)
        stack, child = self.invoke(qualification=[None, {"expiresAt": expires, "evidenceDigest": self.receipt_digest}], phase=lambda *_: b"synthetic")
        with stack, patch.object(r, "now_ms", return_value=stamp):
            r.run(self.directory)
        self.assertEqual([call.args[2] for call in child.call_args_list], ["refresh", "collect", "qualify"])
        self.assertFalse((self.directory / "pending.json").exists())
        results = list((self.directory / "attempts").glob("*/result.json"))
        self.assertEqual(len(results), 1)
        self.assertEqual(r.decode(r.read(results[0]))["qualification"]["expiresAt"], expires)

    def test_explicit_renew_now_still_runs_every_fresh_gate(self):
        stamp = r.now_ms()
        expires = stamp + r.DAY_MS
        self.receipt(stamp, expires)
        stack, child = self.invoke(qualification=[{"expiresAt": expires}, {"expiresAt": expires, "evidenceDigest": self.receipt_digest}], phase=lambda *_: b"synthetic")
        with stack, patch.object(r, "now_ms", return_value=stamp):
            r.run(self.directory, renew_now=True)
        self.assertEqual([call.args[2] for call in child.call_args_list], ["refresh", "collect", "qualify"])
        argv = child.call_args.args[3]
        self.assertEqual(argv[argv.index(r.EXPECTED_GENERATION_FLAG) + 1], self.binding["generation"])

    def test_old_or_extended_receipt_cannot_claim_renewal(self):
        stamp = r.now_ms()
        self.receipt(stamp - 100, stamp + r.DAY_MS)
        stack, _ = self.invoke(qualification=[None, {"expiresAt": stamp + r.DAY_MS, "evidenceDigest": self.receipt_digest}], phase=lambda *_: b"synthetic")
        with stack, patch.object(r, "now_ms", return_value=stamp):
            with self.assertRaises(ValueError):
                r.run(self.directory)
        self.assertTrue((self.directory / "pending.json").exists())
        self.assertEqual(list((self.directory / "attempts").glob("*/result.json")), [])

    def test_post_refresh_signin_drift_cannot_reach_collector(self):
        stack, child = self.invoke(qualification=[None], phase=lambda *_: b"synthetic", verify=[None, ValueError("generation drift")])
        with stack:
            with self.assertRaises(ValueError):
                r.run(self.directory)
        self.assertEqual(child.call_count, 1)
        self.assertTrue((self.directory / "pending.json").exists())

    def test_child_output_limit_and_signal_masks(self):
        code, raw = r.command([sys.executable, "-I", "-c", "import json,signal;print(json.dumps([int(x) for x in signal.pthread_sigmask(signal.SIG_BLOCK,set())]))"],
                              self.root, {"PATH": "/usr/bin:/bin"}, 5, 4096)
        self.assertEqual(code, 0)
        self.assertNotIn(int(signal.SIGTERM), json.loads(raw))
        self.assertNotIn(int(signal.SIGINT), json.loads(raw))
        with self.assertRaises(ValueError):
            r.command([sys.executable, "-I", "-c", "print('x'*4096)"], self.root, {"PATH": "/usr/bin:/bin"}, 5, 1024)

    def test_older_qualifier_cannot_activate_unattended_renewal(self):
        for code, output in ((0, b"Usage: qualify-application\n  --account ID\n"), (1, b"  --expected-generation HEX\n")):
            with patch.object(r, "command", return_value=(code, output)):
                with self.assertRaises(ValueError):
                    r.require_conditional_qualifier(self.binding)
        with patch.object(r, "command", return_value=(0, b"  --expected-generation <HEX>\n")):
            r.require_conditional_qualifier(self.binding)

    def test_capability_read_has_ninety_second_bound_without_retry(self):
        response = {"version": 1, "accounts": [{"id": "a_synthetic", "provider": "claude",
            "enabled": True, "connected": True, "runtimeAdmitted": True,
            "available": False, "busy": False, "qualification": None}]}
        with patch.object(r, "command", return_value=(0, r.encoded(response))) as child:
            self.assertIsNone(r.capabilities(self.binding))
        child.assert_called_once_with(r.xcb_argv(self.binding, "generate", "--capabilities"),
                                      self.binding["source"], self.binding["environment"], 90)
        with patch.object(r, "command", side_effect=ValueError("command deadline")) as child:
            with self.assertRaisesRegex(ValueError, "command deadline"):
                r.capabilities(self.binding)
        self.assertEqual(child.call_count, 1)
        self.assertEqual(child.call_args.args[3], 90)

    def test_capabilities_require_exact_enabled_idle_model_and_bounded_expiry(self):
        expiry = r.now_ms() + 5000
        row = {"id": "a_synthetic", "provider": "claude", "enabled": True, "connected": True,
               "runtimeAdmitted": True, "available": True, "busy": False,
               "models": [{"key": "claude/sonnet/low"}], "qualification": {
                   "runtimeDigest": r.file_hash(self.binary), "evidenceDigest": "7" * 64, "expiresAt": expiry}}
        def query():
            return patch.object(r, "command", return_value=(0, r.encoded({"version": 1, "accounts": [row]})))
        with query():
            self.assertEqual(r.capabilities(self.binding, True)["expiresAt"], expiry)
        row["busy"] = True
        with query(), self.assertRaises(r.AccountBusy):
            r.capabilities(self.binding)
        row["busy"] = False
        row["enabled"] = False
        with query(), self.assertRaises(ValueError):
            r.capabilities(self.binding)
        row["enabled"] = True
        row["qualification"]["expiresAt"] = r.now_ms() + r.DAY_MS + 10_000
        with query(), self.assertRaises(ValueError):
            r.capabilities(self.binding)

    def busy_qualified_row(self, expiry, evidence_digest):
        return {"id": "a_synthetic", "provider": "claude", "enabled": True, "connected": True,
                "runtimeAdmitted": True, "available": False, "busy": True, "reason": "account_busy",
                "models": [{"key": "claude/sonnet/low"}], "qualification": {
                    "runtimeDigest": r.file_hash(self.binary), "evidenceDigest": evidence_digest, "expiresAt": expiry}}

    def test_busy_exemption_only_accepts_complete_valid_qualification(self):
        expiry = r.now_ms() + 5000
        row = self.busy_qualified_row(expiry, "7" * 64)
        def query(value):
            return patch.object(r, "command", return_value=(0, r.encoded({"version": 1, "accounts": [value]})))
        with query(row), self.assertRaises(r.AccountBusy):
            r.capabilities(self.binding, True)
        with query(row):
            self.assertEqual(r.capabilities(self.binding, True, allow_busy=True)["expiresAt"], expiry)
        with query(row), self.assertRaises(ValueError):
            r.capabilities(self.binding, allow_busy=True)
        for field, value in (("enabled", False), ("connected", False), ("runtimeAdmitted", False),
                             ("reason", "application_not_qualified"), ("available", True), ("models", []), ("qualification", None)):
            invalid = {**row, field: value}
            with query(invalid), self.assertRaises(ValueError, msg=field):
                r.capabilities(self.binding, True, allow_busy=True)
        for field, value in (("runtimeDigest", "f" * 64), ("evidenceDigest", "invalid"), ("expiresAt", r.now_ms() - 1)):
            invalid = {**row, "qualification": {**row["qualification"], field: value}}
            with query(invalid), self.assertRaises(ValueError, msg=field):
                r.capabilities(self.binding, True, allow_busy=True)

    def test_turn_started_after_qualification_does_not_retain_completed_intent(self):
        stamp = r.now_ms()
        expires = stamp + r.DAY_MS
        self.receipt(stamp, expires)
        ready = self.busy_qualified_row(expires, self.receipt_digest)
        initial = {**ready, "busy": False, "reason": "application_not_qualified", "qualification": None}
        actual_capabilities = r.capabilities
        stack, child = self.invoke(qualification=actual_capabilities, phase=lambda *_: b"synthetic")
        responses = [(0, r.encoded({"version": 1, "accounts": [row]})) for row in (initial, ready)]
        with stack, patch.object(r, "command", side_effect=responses), patch.object(r, "now_ms", return_value=stamp):
            r.run(self.directory)
        self.assertEqual([call.args[2] for call in child.call_args_list], ["refresh", "collect", "qualify"])
        self.assertFalse((self.directory / "pending.json").exists())
        result = next((self.directory / "attempts").glob("*/result.json"))
        self.assertEqual(r.decode(r.read(result))["qualification"]["evidenceDigest"], self.receipt_digest)

    def test_postvalidation_requires_the_exact_capability_receipt_digest(self):
        stamp = r.now_ms()
        expires = stamp + r.DAY_MS
        self.receipt(stamp, expires)
        stack, _ = self.invoke(qualification=[None, {"expiresAt": expires, "evidenceDigest": "f" * 64}], phase=lambda *_: b"synthetic")
        with stack, patch.object(r, "now_ms", return_value=stamp):
            with self.assertRaises(ValueError):
                r.run(self.directory)
        self.assertTrue((self.directory / "pending.json").exists())
        self.assertEqual(list((self.directory / "attempts").glob("*/result.json")), [])

    def test_source_fingerprint_receives_controlled_incremental_setting(self):
        collector = self.root / "qualification/application-prerequisites.py"
        self.binding["files"][str(collector)] = "a" * 64
        self.binding["environment"] = r.environment(str(self.root), str(self.binary), self.runtimes["node"], self.runtimes["bun"])
        observed = []
        class Collector:
            def source_fingerprint(self, *_):
                observed.append(dict(os.environ))
                return "b" * 64
        previous = dict(os.environ)
        with patch.object(r, "file_hash", return_value="a" * 64), patch.object(r, "collector_module", return_value=Collector()):
            self.assertEqual(r.source_digest(self.binding), "b" * 64)
        self.assertEqual(observed, [self.binding["environment"]])
        self.assertEqual(observed[0]["CARGO_INCREMENTAL"], "0")
        self.assertEqual(dict(os.environ), previous)

    def launch_agents(self):
        library = r.private_directory(self.root / "Library")
        return r.private_directory(library / "LaunchAgents")

    def test_failed_bootstrap_retains_exact_ownership_and_no_immediate_run(self):
        self.launch_agents()
        with patch.object(r.sys, "platform", "darwin"), patch.object(r, "load", return_value=self.binding), \
                patch.object(r, "verify"), patch.object(r, "require_conditional_qualifier"), \
                patch.object(r, "capabilities"), patch.object(r, "command", return_value=(1, b"synthetic bootstrap failure")) as child:
            with self.assertRaises(ValueError):
                r.install(self.directory)
        _, path, raw = r.job(self.binding, self.directory)
        self.assertEqual(r.read(path), raw)
        self.assertTrue((self.directory / "launchd.json").exists())
        self.assertEqual(child.call_count, 1)
        self.assertEqual(child.call_args.args[0][1], "bootstrap")

    def test_uninstall_refuses_changed_plist_before_launchctl(self):
        self.launch_agents()
        label, path, raw = r.job(self.binding, self.directory)
        r.write_once(path, b"foreign changed job")
        r.write_once(self.directory / "launchd.json", r.encoded({"label": label, "path": str(path), "sha256": r.sha(raw)}))
        with patch.object(r.sys, "platform", "darwin"), patch.object(r, "load", return_value=self.binding), patch.object(r, "command") as child:
            with self.assertRaises(ValueError):
                r.uninstall(self.directory)
        child.assert_not_called()
        self.assertEqual(r.read(path), b"foreign changed job")

    def test_uninstall_refuses_prior_background_profile_without_changing_it(self):
        self.launch_agents()
        label, path, current = r.job(self.binding, self.directory)
        previous = plistlib.loads(current)
        previous.update(ProcessType="Background", LowPriorityIO=True)
        raw = plistlib.dumps(previous, sort_keys=True)
        r.write_once(path, raw)
        r.write_once(self.directory / "launchd.json", r.encoded({"label": label, "path": str(path), "sha256": r.sha(raw)}))
        with patch.object(r.sys, "platform", "darwin"), patch.object(r, "load", return_value=self.binding), patch.object(r, "command") as child:
            with self.assertRaisesRegex(ValueError, "launchd ownership changed"):
                r.uninstall(self.directory)
        child.assert_not_called()
        self.assertEqual(r.read(path), raw)
        self.assertTrue((self.directory / "launchd.json").exists())

    def test_uninstall_preserves_binding_and_evidence(self):
        self.launch_agents()
        label, path, raw = r.job(self.binding, self.directory)
        r.write_once(path, raw)
        r.write_once(self.directory / "launchd.json", r.encoded({"label": label, "path": str(path), "sha256": r.sha(raw)}))
        r.write_once(self.directory / "pending.json", b'{"attempt":"preserve"}')
        with patch.object(r.sys, "platform", "darwin"), patch.object(r, "load", return_value=self.binding), \
                patch.object(r, "command", return_value=(0, b"")) as child, contextlib.redirect_stdout(io.StringIO()):
            r.uninstall(self.directory)
        child.assert_called_once_with(["/bin/launchctl", "bootout", "gui/" + str(os.getuid()), str(path)],
                                      self.binding["home"], self.binding["environment"], 90, r.MAX_JSON)
        self.assertFalse(path.exists())
        self.assertFalse((self.directory / "launchd.json").exists())
        self.assertEqual(r.read(self.directory / "pending.json"), b'{"attempt":"preserve"}')
        self.assertTrue((self.directory / "attempts").is_dir())


if __name__ == "__main__":
    unittest.main(verbosity=1)
