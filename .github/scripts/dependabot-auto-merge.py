#!/usr/bin/env python3
"""Clear old auto-merge requests before Check's Required gate; merge its checked head."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

SHA = re.compile(r"[0-9a-f]{40}\Z")
REPO = "hraness/xcb"
QUERY = """query($number: Int!) {
  repository(owner: "hraness", name: "xcb") {
    pullRequest(number: $number) {
      id number state isDraft headRefOid headRefName baseRefName
      headRepository { nameWithOwner }
      autoMergeRequest { enabledAt }
    }
  }
}"""
DISABLE = """mutation($id: ID!) {
  disablePullRequestAutoMerge(input: {pullRequestId: $id}) {
    pullRequest { autoMergeRequest { enabledAt } }
  }
}"""


def find_pr(event, repo, list_pulls, phase):
    """Return an open, exact-head Dependabot PR for this Check phase, or nothing."""
    run = event.get("workflow_run", {})
    sha = run.get("head_sha", "")
    if (repo != REPO or event.get("action") != phase or
            run.get("name") != "Check" or run.get("event") != "pull_request" or
            (phase == "completed" and run.get("conclusion") != "success") or
            (phase == "requested" and run.get("conclusion") is not None) or
            not isinstance(sha, str) or not SHA.fullmatch(sha)):
        return None

    # workflow_run.pull_requests can be empty even for same-repo Dependabot
    # PRs. The commit association API includes those PRs (including closed
    # ones), so verify the live head and state instead of trusting that array.
    pulls = list_pulls(repo, sha)
    if not isinstance(pulls, list) or len(pulls) >= 100:
        raise ValueError("incomplete or invalid pull-request lookup")
    candidates = [pr for pr in pulls if
                  pr.get("state") == "open" and not pr.get("draft") and
                  pr.get("user", {}).get("login") == "dependabot[bot]" and
                  pr.get("base", {}).get("ref") == "main" and
                  pr.get("head", {}).get("sha") == sha and
                  pr.get("head", {}).get("ref", "").startswith("dependabot/") and
                  (pr.get("head", {}).get("repo") or {}).get("full_name") == repo and
                  type(pr.get("number")) is int and pr["number"] > 0]
    return (candidates[0]["number"], sha) if len(candidates) == 1 else None


def select(event, repo, list_pulls):
    """Only a successfully completed Check may authorize an app-token merge."""
    return find_pr(event, repo, list_pulls, "completed")


def list_pulls(repo, sha):
    return json.loads(subprocess.check_output(
        ["gh", "api", "-H", "Accept: application/vnd.github+json",
         f"repos/{repo}/commits/{sha}/pulls?per_page=100"], text=True))


def graphql(query, variable, value):
    return json.loads(subprocess.check_output(
        ["gh", "api", "graphql", "-f", f"query={query}",
         "-F" if variable == "number" else "-f", f"{variable}={value}"], text=True))


def get_pr(number):
    return graphql(QUERY, "number", number)["data"]["repository"]["pullRequest"]


def same_head(pr, repo, number, sha):
    return (isinstance(pr, dict) and pr.get("number") == number and
            pr.get("state") == "OPEN" and pr.get("isDraft") is False and
            pr.get("baseRefName") == "main" and pr.get("headRefOid") == sha and
            pr.get("headRefName", "").startswith("dependabot/") and
            (pr.get("headRepository") or {}).get("nameWithOwner") == repo and
            isinstance(pr.get("id"), str) and bool(pr["id"]) and
            "autoMergeRequest" in pr)


def disable_preexisting(event, repo, list_pulls, read_pr, disable):
    """On Check request, cancel any GITHUB_TOKEN auto-merge before Required is green."""
    selected = find_pr(event, repo, list_pulls, "requested")
    if selected is None:
        return False
    number, sha = selected
    pr = read_pr(number)
    if not same_head(pr, repo, number, sha):
        raise ValueError("Dependabot head changed before auto-merge cleanup")
    if pr["autoMergeRequest"] is None:
        return False
    # No merge here: a pending auto-merge can race the workflow_run completed
    # event and suppress push CI. Required waits until this request is gone.
    result = disable(pr["id"])
    if result["data"]["disablePullRequestAutoMerge"]["pullRequest"]["autoMergeRequest"] is not None:
        raise ValueError("auto-merge request was not disabled")
    print(f"Disabled pre-existing auto-merge for Dependabot PR #{number}.")
    return True


def disable_request(pr_id):
    return graphql(DISABLE, "id", pr_id)


def guard(event, repo, read_pr, sleep=time.sleep, attempts=25):
    """Fail Required closed while a bot-token auto-merge can race app-token merge."""
    pr = event.get("pull_request")
    if repo != REPO or not isinstance(pr, dict) or pr.get("user", {}).get("login") != "dependabot[bot]":
        return
    number = pr.get("number")
    sha = pr.get("head", {}).get("sha")
    if type(number) is not int or number <= 0 or not isinstance(sha, str) or not SHA.fullmatch(sha):
        raise ValueError("invalid Dependabot PR event")
    for attempt in range(attempts):
        live = read_pr(number)
        if not same_head(live, repo, number, sha):
            raise ValueError("Dependabot PR is no longer at the checked head")
        if live["autoMergeRequest"] is None:
            print(f"Dependabot PR #{number} has no competing auto-merge request.")
            return
        if attempt + 1 < attempts:
            sleep(5)
    raise ValueError("competing auto-merge request is still enabled; refusing Required")


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if argv not in ([], ["disable"], ["guard"]):
        raise ValueError("expected disable, guard, or no argument")
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    repo = os.environ["GITHUB_REPOSITORY"]
    if argv == ["guard"]:
        guard(event, repo, get_pr)
        return
    if argv == ["disable"]:
        disable_preexisting(event, repo, list_pulls, get_pr, disable_request)
        return
    result = select(event, repo, list_pulls)
    if result is None:
        print("No open Dependabot PR at the successful Check head; no merge.")
        return
    number, sha = result
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"number={number}\nsha={sha}\n")
    print(f"Selected Dependabot PR #{number} at {sha}; branch policy decides the merge.")


if __name__ == "__main__":
    main()
