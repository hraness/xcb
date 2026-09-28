#!/usr/bin/env python3
"""Offline tests for the Codex catalog workflow helper: no npm, gh, or Codex calls."""
import base64
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("catalog", Path(__file__).with_name("codex-catalog.py"))
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)

SHA = "27ceb5f9b957b43a519efe4eaa3816a0bffb0a531a2c89af18840c0a3c016a7d"
CATALOG = {"version": 1, "claude": [], "codex": [{"version": "0.156.1", "sha256": "0" * 64, "platform": "darwin-aarch64", "qualifiedBy": "release"}], "devin": []}


class FakeGitHub:
    def __init__(self, issues=()):
        self.issues, self.calls, self.next = [dict(issue) for issue in issues], [], 400

    def catalog_issues(self):
        return [dict(issue) for issue in self.issues]

    def create(self, title, body):
        self.calls.append(("create", title, body))
        self.next += 1
        return self.next

    def update(self, number, title, body):
        self.calls.append(("update", number, title, body))

    def close(self, number, comment, reason="completed"):
        self.calls.append(("close", number, comment, reason))


def issue(number, version, outcome="incompatible"):
    return {"number": number, "title": f"Codex {version}", "version": version,
            "body": f"<!-- xcb-codex-catalog version={version} outcome={outcome} -->\nold\n"}


class CatalogTests(unittest.TestCase):
    def setUp(self):
        # report() prints workflow commands; keep them out of the test log,
        # where GitHub would turn them into annotations.
        self.printed = patch("builtins.print").start()
        self.addCleanup(patch.stopall)
        self.temp = tempfile.TemporaryDirectory(prefix="xcb-codex-catalog-")
        self.root = Path(self.temp.name)
        (self.root / "qualified-builds.json").write_text(json.dumps(CATALOG))
        self.release_source("0.156.1", "0" * 64)
        self.evidence = self.root / "inventory-out"
        self.evidence.mkdir()
        self.env = {"VERSION": "0.157.1", "SHA256": SHA, "NEEDED": "true", "SOURCE": "latest", "INVENTORY_DIR": str(self.evidence),
                    "PACKAGE": c.PACKAGE_URL.format(version="0.157.1"), "RUN_URL": "https://github.com/hraness/xcb/actions/runs/36319874678",
                    "RUN_ID": "36319874678", "ARTIFACT": "codex-inventory-0.157.1-1", "GITHUB_STEP_SUMMARY": str(self.root / "summary.md")}

    def tearDown(self):
        self.temp.cleanup()

    def release_source(self, version, digest, retained=()):
        path = self.root / "crates/xcb-runtime/src/codex/config.rs"
        path.parent.mkdir(parents=True, exist_ok=True)
        old = "\n".join(f'("{v}", "{d}", "{"f" * 64}"),' for v, d in retained)
        path.write_text(f'pub const VERSION: &str = "{version}";\n'
                        f'pub const BINARY_SHA256: &str = "{digest}";\n'
                        'pub const REVIEWED_BUILDS: &[(&str, &str, &str)] = &[\n'
                        '(VERSION, BINARY_SHA256, SCHEMA_SHA256),\n' + old + '\n];\n')

    def registry_fixture(self, binary):
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
            member = tarfile.TarInfo(c.BINARY_MEMBER)
            member.size = len(binary)
            archive.addfile(member, io.BytesIO(binary))
        tarball = buffer.getvalue()
        url = c.PACKAGE_URL.format(version="0.157.1")
        integrity = "sha512-" + base64.b64encode(hashlib.sha512(tarball).digest()).decode()
        metadata = {"dist-tags": {"latest": "0.157.1"}, "versions": {"0.157.1-darwin-arm64": {"dist": {"tarball": url, "integrity": integrity}}}}
        return {c.REGISTRY: json.dumps(metadata).encode(), url: tarball}

    def verdict(self, outcome, status, **detail):
        record = {"schema": "xcb.codex-inventory-verdict.v1", "outcome": outcome, "version": "0.157.1", "binarySha256": SHA, **detail}
        (self.evidence / "verdict.json").write_text(json.dumps(record))
        self.env["INVENTORY_STATUS"] = str(status)

    def test_version_grammar_matches_the_runtime_catalog(self):
        for good in ["0.157.1", "0.159.0-alpha.9", "10.0.0-rc-1.2"]:
            self.assertTrue(c.VERSION.fullmatch(good), good)
        for bad in ["latest", "0.157", "01.2.3", "0.157.1 ", "0.157.1;x", "0.157.1+build", "../0.157.1", "0.157.1\n"]:
            self.assertFalse(c.VERSION.fullmatch(bad), bad)

    def test_catalogued_versions_close_their_issues_and_nothing_else(self):
        github = FakeGitHub([issue(10, "0.156.1"), issue(11, "0.157.1")])
        self.env.update(NEEDED="false", VERSION="0.156.1")
        self.assertEqual(c.report(self.env, github, self.root), 0)
        self.assertEqual([call[:2] for call in github.calls], [("close", 10)])
        self.assertEqual(github.calls[0][3], "completed")
        self.assertIn("nothing to do", (self.root / "summary.md").read_text())

    def test_schema_drift_is_an_expected_outcome_that_needs_an_xcb_release(self):
        self.verdict("incompatible", 3, reason="schema-drift", observedSchemaSha256="4" * 64, bakedSchemaSha256="0" * 64, bakedVersion="0.156.1")
        github = FakeGitHub([issue(10, "0.157.0")])
        self.assertEqual(c.report(self.env, github, self.root), 0)
        action, title, body = github.calls[0]
        self.assertEqual(action, "create")
        self.assertIn("needs an xcb release", title)
        self.assertTrue(body.startswith("<!-- xcb-codex-catalog version=0.157.1 outcome=incompatible -->\n"))
        for fact in [SHA, "4" * 64, "0" * 64, "SCHEMA_SHA256", self.env["RUN_URL"], self.env["ARTIFACT"]]:
            self.assertIn(fact, body)
        self.assertEqual(github.calls[1][:2], ("close", 10))
        self.assertIn("#401", github.calls[1][2])
        self.assertEqual(github.calls[1][3], "not_planned")
        self.assertIn("::warning::", self.printed.call_args.args[0])

    def test_an_explicit_version_never_closes_other_issues(self):
        self.verdict("incompatible", 3, reason="schema-drift", observedSchemaSha256="4" * 64, bakedSchemaSha256="0" * 64, bakedVersion="0.156.1")
        self.env["SOURCE"] = "input"
        github = FakeGitHub([issue(10, "0.158.0")])
        self.assertEqual(c.report(self.env, github, self.root), 0)
        self.assertEqual([call[0] for call in github.calls], ["create"])

    def test_the_same_version_updates_its_issue_instead_of_opening_another(self):
        self.verdict("incompatible", 3, reason="schema-drift", observedSchemaSha256="4" * 64, bakedSchemaSha256="0" * 64, bakedVersion="0.156.1")
        github = FakeGitHub([issue(12, "0.157.1")])
        self.assertEqual(c.report(self.env, github, self.root), 0)
        self.assertEqual([call[:2] for call in github.calls], [("update", 12)])
        title, body = c.render(json.loads((self.evidence / "verdict.json").read_text()), self.env)
        github = FakeGitHub([{"number": 12, "version": "0.157.1", "title": title, "body": body}])
        self.assertEqual(c.report(self.env, github, self.root), 0)
        self.assertEqual(github.calls, [])

    def test_a_passing_build_carries_the_reviewed_entry_for_a_pull_request(self):
        self.verdict("passed", 0, cases=12, failures=[], inventory="inventory.json", inventorySha256="f" * 64, schemaSha256="0" * 64)
        github = FakeGitHub()
        self.assertEqual(c.report(self.env, github, self.root), 0)
        _, title, body = github.calls[0]
        self.assertIn("add it to qualified-builds.json", title)
        entry = json.loads(body.split("```json\n", 1)[1].split("\n```", 1)[0])
        self.assertEqual(entry, {"version": "0.157.1", "sha256": SHA, "platform": "darwin-aarch64", "qualifiedBy": "codex-catalog-run-36319874678"})
        self.assertLessEqual(len(entry["qualifiedBy"]), 64)
        self.assertIn("qualification/codex-0.157.1-inventory.json", body)

    def test_release_supported_build_never_becomes_a_shared_catalog_proposal(self):
        self.release_source("0.157.1", SHA)
        self.verdict("passed", 0, cases=12, failures=[])
        github = FakeGitHub([issue(10, "0.157.1"), issue(11, "0.158.0")])
        # Even an inconsistent NEEDED=true output cannot create a passing
        # proposal for an exact build already owned by the release adapter.
        self.assertEqual(c.report(self.env, github, self.root), 0)
        self.assertEqual([call[:2] for call in github.calls], [("close", 10)])
        self.assertIn("older xcb releases", github.calls[0][2])
        self.assertIn("no shared-catalog proposal", (self.root / "summary.md").read_text())
        self.assertEqual(json.loads((self.root / "qualified-builds.json").read_text()), CATALOG)

    def test_failed_cases_are_listed_with_candidate_values_kept_inert(self):
        case = {"model": "gpt-5.5", "reasoningEffort": "high`](https://example.com)\n# x", "exactDynamicManifestVerified": False,
                "emptyManifestVerified": True, "permittedCallbackVerified": True, "forgedBuiltinRejections": 8, "rootExitCode": 0}
        self.verdict("failed", 1, cases=12, failures=[{"case": case, "violations": ["x"], "rejections": []}])
        github = FakeGitHub()
        self.assertEqual(c.report(self.env, github, self.root), 0)
        body = github.calls[0][2]
        self.assertIn("1 of 12 cases failed", body)
        self.assertIn("exact dynamic tool manifest", body)
        line = next(line for line in body.splitlines() if "gpt-5.5" in line)
        self.assertEqual(line.count("`"), 6)
        self.assertNotIn("\n# x", body)

    def test_a_missing_or_inconsistent_verdict_fails_without_writing(self):
        github = FakeGitHub([issue(10, "0.157.0")])
        self.env["INVENTORY_STATUS"] = "1"
        self.assertEqual(c.report(self.env, github, self.root), 1)
        self.verdict("passed", 1, cases=12, failures=[])
        self.assertEqual(c.report(self.env, github, self.root), 1)
        self.verdict("incompatible", 3, reason="schema-drift")
        self.env["SHA256"] = "1" * 64
        self.assertEqual(c.report(self.env, github, self.root), 1)
        self.assertEqual(github.calls, [])

    def test_only_open_bot_issues_with_the_marker_are_tracked(self):
        pages = [[
            {"number": 1, "title": "a", "body": "<!-- xcb-codex-catalog version=0.157.1 outcome=passed -->\n", "user": {"login": c.BOT}},
            {"number": 2, "title": "b", "body": "<!-- xcb-codex-catalog version=0.157.1 outcome=passed -->\n", "user": {"login": "someone"}},
            {"number": 3, "title": "c", "body": "<!-- xcb-codex-catalog version=0.157.1 outcome=passed -->\n", "user": {"login": c.BOT}, "pull_request": {}},
            {"number": 4, "title": "d", "body": None, "user": {"login": c.BOT}},
        ]]

        class Result:
            stdout = json.dumps(pages)

        calls = []
        github = c.GitHub("hraness/xcb", run=lambda args, **kwargs: calls.append(args) or Result())
        self.assertEqual([found["number"] for found in github.catalog_issues()], [1])
        self.assertEqual(calls[0][:4], ["gh", "api", "--paginate", "--slurp"])

    def test_resolve_stages_the_exact_binary_after_the_registry_integrity_check(self):
        binary = b"\xcf\xfa\xed\xfe synthetic codex"
        responses = self.registry_fixture(binary)
        url = c.PACKAGE_URL.format(version="0.157.1")
        outputs = self.root / "outputs"
        with patch.object(c, "fetch", lambda address, limit=0: responses[address]):
            c.resolve({"GITHUB_OUTPUT": str(outputs)}, self.root)
            self.assertEqual((self.root / "candidate/codex").read_bytes(), binary)
            self.assertEqual((self.root / "candidate/codex").stat().st_mode & 0o777, 0o700)
            written = dict(line.split("=", 1) for line in outputs.read_text().splitlines())
            self.assertEqual(written, {"needed": "true", "version": "0.157.1", "sha256": hashlib.sha256(binary).hexdigest(), "package": url, "source": "latest"})
            outputs.write_text("")
            c.resolve({"GITHUB_OUTPUT": str(outputs), "REQUESTED_VERSION": "0.156.1"}, self.root)
            self.assertIn("needed=false", outputs.read_text())
            responses[url] += b"tampered"
            with self.assertRaises(SystemExit):
                c.resolve({"GITHUB_OUTPUT": str(outputs)}, self.root)
            with self.assertRaises(SystemExit):
                c.resolve({"GITHUB_OUTPUT": str(outputs), "REQUESTED_VERSION": "latest"}, self.root)

    def test_resolve_skips_only_exact_current_and_retained_release_artifacts(self):
        binary = b"\xcf\xfa\xed\xfe synthetic release codex"
        digest = hashlib.sha256(binary).hexdigest()
        responses = self.registry_fixture(binary)
        outputs = self.root / "outputs"
        with patch.object(c, "fetch", lambda address, limit=0: responses[address]):
            for retained in [False, True]:
                with self.subTest(retained=retained):
                    if retained:
                        self.release_source("0.158.0", "1" * 64, [("0.157.1", digest)])
                    else:
                        self.release_source("0.157.1", digest)
                    outputs.write_text("")
                    c.resolve({"GITHUB_OUTPUT": str(outputs)}, self.root)
                    written = dict(line.split("=", 1) for line in outputs.read_text().splitlines())
                    self.assertEqual(written, {"needed": "false", "version": "0.157.1", "sha256": digest, "source": "latest"})
                    self.assertFalse((self.root / "candidate").exists())
            # A reused version with different bytes is a new candidate.
            self.release_source("0.157.1", "1" * 64)
            outputs.write_text("")
            c.resolve({"GITHUB_OUTPUT": str(outputs)}, self.root)
            self.assertIn("needed=true", outputs.read_text())
            self.assertEqual((self.root / "candidate/codex").read_bytes(), binary)
            # A known tuple still requires registry integrity verification.
            self.release_source("0.157.1", digest)
            responses[c.PACKAGE_URL.format(version="0.157.1")] += b"tampered"
            with self.assertRaises(SystemExit):
                c.resolve({"GITHUB_OUTPUT": str(outputs)}, self.root)


if __name__ == "__main__":
    unittest.main()
