#!/usr/bin/env python3
"""Offline regressions for Dependabot's exact-head, app-token merge gate."""

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


def event(action="completed", **updates):
    run = {"name": "Check", "event": "pull_request", "conclusion": "success" if action == "completed" else None,
           "head_sha": SHA, "pull_requests": []}
    run.update(updates)
    return {"action": action, "workflow_run": run}


def pr(**updates):
    result = {"number": 522, "state": "open", "draft": False,
              "user": {"login": "dependabot[bot]"}, "base": {"ref": "main"},
              "head": {"sha": SHA, "ref": "dependabot/github_actions/example",
                       "repo": {"full_name": merge.REPO}}}
    result.update(updates)
    return result


def live(**updates):
    result = {"id": "PR_node_id", "number": 522, "state": "OPEN", "isDraft": False,
              "author": {"login": "dependabot[bot]"}, "baseRefName": "main",
              "headRefOid": SHA, "headRefName": "dependabot/github_actions/example",
              "headRepository": {"nameWithOwner": merge.REPO},
              "autoMergeRequest": {"enabledAt": "2026-10-17T00:00:00Z"}}
    result.update(updates)
    return result


def required_event():
    return {"pull_request": pr()}


class DependabotMergeTests(unittest.TestCase):
    def test_empty_workflow_run_pr_array_still_finds_the_exact_head(self):
        calls = []
        def lookup(repo, sha):
            calls.append((repo, sha))
            return [pr()]
        self.assertEqual(merge.select(event(), merge.REPO, lookup), (522, SHA))
        self.assertEqual(calls, [(merge.REPO, SHA)])

    def test_only_successful_completion_selects_a_merge(self):
        def no_lookup(*args):
            self.fail("should not query PRs")
        for changed in ({"conclusion": "failure"}, {"event": "push"},
                        {"name": "Other"}, {"head_sha": "bad"}):
            self.assertIsNone(merge.select(event(**changed), merge.REPO, no_lookup))
        self.assertIsNone(merge.select(event("requested"), merge.REPO, no_lookup))
        self.assertIsNone(merge.select(event("in_progress"), merge.REPO, no_lookup))
        self.assertIsNone(merge.select(event(), "someone/fork", no_lookup))
        self.assertIsNone(merge.find_pr(event("requested", conclusion="success"), merge.REPO,
                                        no_lookup, "requested"))

    def test_stale_untrusted_or_closed_pr_is_not_selected(self):
        head = pr()["head"]
        for candidate in (pr(state="closed"), pr(draft=True),
                          pr(user={"login": "someone"}), pr(base={"ref": "other"}),
                          pr(head={**head, "sha": "0" * 40}),
                          pr(head={**head, "repo": {"full_name": "someone/fork"}}),
                          pr(head={**head, "ref": "not-dependabot"})):
            self.assertIsNone(merge.select(event(), merge.REPO, lambda *_: [candidate]))

    def test_ambiguous_or_incomplete_commit_associations_fail_closed(self):
        for action in ("requested", "completed"):
            self.assertIsNone(merge.find_pr(event(action), merge.REPO,
                                             lambda *_: [pr(), pr(number=523)], action))
            with self.assertRaises(ValueError):
                merge.find_pr(event(action), merge.REPO, lambda *_: [pr()] * 100, action)

    def test_preexisting_auto_merge_is_disabled_before_required_can_pass(self):
        ids = []
        def disable(pr_id):
            ids.append(pr_id)
            return {"data": {"disablePullRequestAutoMerge": {
                "pullRequest": {"autoMergeRequest": None}}}}
        self.assertTrue(merge.disable_preexisting(event("requested"), merge.REPO,
                        lambda *_: [pr()], lambda _: live(), disable))
        self.assertEqual(ids, ["PR_node_id"])
        self.assertFalse(merge.disable_preexisting(event("requested"), merge.REPO,
                         lambda *_: [pr()], lambda _: live(autoMergeRequest=None), disable))
        self.assertFalse(merge.disable_preexisting(event(), merge.REPO,
                         lambda *_: self.fail("completed must not disable"),
                         lambda _: self.fail("completed must not fetch"), disable))
        self.assertEqual(ids, ["PR_node_id"])

    def test_stale_head_never_disables_an_unrelated_auto_merge(self):
        with self.assertRaisesRegex(ValueError, "head changed"):
            merge.disable_preexisting(event("requested"), merge.REPO, lambda *_: [pr()],
                                      lambda _: live(headRefOid="0" * 40),
                                      lambda _: self.fail("stale head cannot mutate"))
        with self.assertRaisesRegex(ValueError, "not disabled"):
            merge.disable_preexisting(event("requested"), merge.REPO, lambda *_: [pr()],
                                      lambda _: live(), lambda _: {"data": {
                                          "disablePullRequestAutoMerge": {"pullRequest": {
                                              "autoMergeRequest": {"enabledAt": "still-on"}}}}})

    def test_required_waits_for_cleanup_then_passes_exact_pr_head(self):
        reads, sleeps = [], []
        def read(number):
            reads.append(number)
            return live() if len(reads) == 1 else live(autoMergeRequest=None)
        merge.guard(required_event(), merge.REPO, read, sleeps.append, attempts=2)
        self.assertEqual(reads, [522, 522])
        self.assertEqual(sleeps, [5])

    def test_required_never_goes_green_with_a_competing_request(self):
        with self.assertRaisesRegex(ValueError, "competing auto-merge"):
            merge.guard(required_event(), merge.REPO, lambda _: live(),
                        lambda _: self.fail("one attempt must not sleep"), attempts=1)
        with self.assertRaisesRegex(ValueError, "checked head"):
            merge.guard(required_event(), merge.REPO,
                        lambda _: live(headRefOid="0" * 40), attempts=1)
        with self.assertRaisesRegex(ValueError, "checked head"):
            merge.guard(required_event(), merge.REPO,
                        lambda _: {k: v for k, v in live().items() if k != "autoMergeRequest"},
                        attempts=1)
        merge.guard({}, merge.REPO, lambda _: self.fail("non-PR must not query"))
        merge.guard(required_event(), "someone/fork", lambda _: self.fail("other repo must not query"))

    def test_main_emits_only_validated_numeric_number_and_sha_on_completion(self):
        with tempfile.TemporaryDirectory() as directory:
            event_file = Path(directory) / "event.json"
            output_file = Path(directory) / "output"
            event_file.write_text(json.dumps(event()))
            env = {"GITHUB_EVENT_PATH": str(event_file), "GITHUB_OUTPUT": str(output_file),
                   "GITHUB_REPOSITORY": merge.REPO}
            with patch.dict(merge.os.environ, env), patch.object(merge, "list_pulls", return_value=[pr()]):
                merge.main([])
            self.assertEqual(output_file.read_text(), f"number=522\nsha={SHA}\n")
            output_file.unlink()
            event_file.write_text(json.dumps(event("requested")))
            with patch.dict(merge.os.environ, env), patch.object(merge, "list_pulls",
                    side_effect=lambda *_: self.fail("requested must not select merge")):
                merge.main([])
            self.assertFalse(output_file.exists())


if __name__ == "__main__":
    unittest.main()
