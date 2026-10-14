#!/usr/bin/env python3
"""Offline regression tests for Dependabot's exact-head, app-token merge gate."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("dependabot_merge", Path(__file__).with_name("dependabot-auto-merge.py"))
merge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(merge)
SHA = "653440960d009f49bc1b82230eaef195005a4871"


def event(**updates):
    run = {"name": "Check", "event": "pull_request", "conclusion": "success",
           "head_sha": SHA, "pull_requests": []}
    run.update(updates)
    return {"workflow_run": run}


def pr(**updates):
    result = {"number": 522, "state": "open", "draft": False,
              "user": {"login": "dependabot[bot]"}, "base": {"ref": "main"},
              "head": {"sha": SHA, "ref": "dependabot/github_actions/example",
                       "repo": {"full_name": merge.REPO}}}
    result.update(updates)
    return result


class DependabotMergeTests(unittest.TestCase):
    def test_empty_workflow_run_pr_array_still_finds_the_exact_head(self):
        calls = []
        def lookup(repo, sha):
            calls.append((repo, sha))
            return [pr()]
        self.assertEqual(merge.select(event(), merge.REPO, lookup), (522, SHA))
        self.assertEqual(calls, [(merge.REPO, SHA)])

    def test_failed_or_non_pr_check_never_queries_or_merges(self):
        def no_lookup(*args):
            self.fail("should not query PRs")
        for changed in ({"conclusion": "failure"}, {"event": "push"},
                        {"name": "Other"}, {"head_sha": "bad"}):
            self.assertIsNone(merge.select(event(**changed), merge.REPO, no_lookup))
        self.assertIsNone(merge.select(event(), "someone/fork", no_lookup))

    def test_stale_untrusted_or_closed_pr_is_not_selected(self):
        head = pr()["head"]
        for candidate in (pr(state="closed"), pr(draft=True),
                          pr(user={"login": "someone"}), pr(base={"ref": "other"}),
                          pr(head={**head, "sha": "0" * 40}),
                          pr(head={**head, "repo": {"full_name": "someone/fork"}}),
                          pr(head={**head, "ref": "not-dependabot"})):
            self.assertIsNone(merge.select(event(), merge.REPO, lambda *_: [candidate]))

    def test_ambiguous_or_incomplete_commit_associations_fail_closed(self):
        self.assertIsNone(merge.select(event(), merge.REPO, lambda *_: [pr(), pr(number=523)]))
        with self.assertRaises(ValueError):
            merge.select(event(), merge.REPO, lambda *_: [pr()] * 100)

    def test_main_emits_only_validated_numeric_number_and_sha(self):
        with tempfile.TemporaryDirectory() as directory:
            event_file = Path(directory) / "event.json"
            output_file = Path(directory) / "output"
            event_file.write_text(json.dumps(event()))
            env = {"GITHUB_EVENT_PATH": str(event_file), "GITHUB_OUTPUT": str(output_file),
                   "GITHUB_REPOSITORY": merge.REPO}
            with patch.dict(merge.os.environ, env), patch.object(merge, "list_pulls", return_value=[pr()]):
                merge.main()
            self.assertEqual(output_file.read_text(), f"number=522\nsha={SHA}\n")


if __name__ == "__main__":
    unittest.main()
