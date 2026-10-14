#!/usr/bin/env python3
"""Select an exact-head Dependabot PR after its Check workflow succeeds."""

import json
import os
from pathlib import Path
import re
import subprocess

SHA = re.compile(r"[0-9a-f]{40}\Z")
REPO = "hraness/xcb"


def select(event, repo, list_pulls):
    """Return (PR number, checked head SHA), or nothing; never trust a PR title."""
    run = event.get("workflow_run", {})
    sha = run.get("head_sha", "")
    if (repo != REPO or run.get("name") != "Check" or
            run.get("event") != "pull_request" or
            run.get("conclusion") != "success" or
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


def list_pulls(repo, sha):
    return json.loads(subprocess.check_output(
        ["gh", "api", "-H", "Accept: application/vnd.github+json",
         f"repos/{repo}/commits/{sha}/pulls?per_page=100"], text=True))


def main():
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    result = select(event, os.environ["GITHUB_REPOSITORY"], list_pulls)
    if result is None:
        print("No open Dependabot PR at the successful Check head; no merge.")
        return
    number, sha = result
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"number={number}\nsha={sha}\n")
    print(f"Selected Dependabot PR #{number} at {sha}; branch policy decides the merge.")


if __name__ == "__main__":
    main()
