#!/usr/bin/env python3
"""Steps of the Codex catalog workflow (.github/workflows/codex-catalog.yml).

resolve  Pick the npm `latest` Codex version, or REQUESTED_VERSION. When
         qualified-builds.json does not list it, download the darwin-arm64
         package, check its registry integrity, stage candidate/codex, and
         write its SHA-256 to the step outputs. Exact release-supported
         artifacts are excluded from shared-catalog proposals.
report   Turn the inventory verdict into the run summary and one tracking
         issue per Codex version. The expected outcomes (passed, failed,
         incompatible) exit 0; a missing or inconsistent verdict exits 1.

The workflow never edits qualified-builds.json: GitHub Actions cannot open
pull requests in this repository, and a pull request opened with
GITHUB_TOKEN would not run the Required check. A passing build's issue
carries the reviewed entry for a maintainer's pull request instead.
"""
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import urllib.request

REGISTRY = "https://registry.npmjs.org/@openai%2Fcodex"
PACKAGE_URL = "https://registry.npmjs.org/@openai/codex/-/codex-{version}-darwin-arm64.tgz"
BINARY_MEMBER = "package/vendor/aarch64-apple-darwin/bin/codex"
# The version grammar crates/xcb-runtime/src/catalog.rs accepts, without build metadata.
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?")
DIGEST = re.compile(r"[0-9a-f]{64}")
MARKER = re.compile(r"^<!-- xcb-codex-catalog version=(\S+) outcome=(\S+) -->$", re.M)
BOT = "github-actions[bot]"
EXIT_CODES = {"passed": 0, "failed": 1, "incompatible": 3}
LIMIT = 256 * 1024 * 1024


def fail(message):
    print(f"::error::{message}")
    raise SystemExit(1)


def fetch(url, limit=LIMIT):
    with urllib.request.urlopen(url, timeout=120) as response:
        data = response.read(limit + 1)
    if len(data) > limit:
        fail(f"{url} exceeds {limit} bytes")
    return data


def catalogued(catalog, version):
    return any(entry.get("version") == version for entry in catalog.get("codex", []))


def release_builds(root):
    """Exact source-baked pairs, including retained release-only adapters.

    A new adapter can support a changed schema that older xcb releases cannot.
    Those older clients also read the shared catalog, so these pairs must not
    become catalog proposals merely because the current inventory now passes.
    """
    source = (root / "crates/xcb-runtime/src/codex/config.rs").read_text()
    values = []
    for name in ["VERSION", "BINARY_SHA256"]:
        found = re.findall(r'^pub const ' + name + r': &str = "([^"]+)";', source, re.M)
        if len(found) != 1:
            fail("Codex release constants are missing or ambiguous")
        values.append(found[0])
    if not VERSION.fullmatch(values[0]) or not DIGEST.fullmatch(values[1]):
        fail("Codex release constants are invalid")
    pairs = {tuple(values)}
    block = re.search(r'pub const REVIEWED_BUILDS: &\[\(&str, &str, &str\)\] = &\[(.*?)\];', source, re.S)
    if block is None:
        fail("Codex reviewed release builds are missing")
    literals = re.findall(r'"([^"]*)"', block.group(1))
    if len(literals) % 3:
        fail("Codex reviewed release bindings are invalid")
    for version, digest, schema in zip(*[iter(literals)] * 3):
        if not VERSION.fullmatch(version) or not DIGEST.fullmatch(digest) or not DIGEST.fullmatch(schema):
            fail("Codex reviewed release binding is invalid")
        pairs.add((version, digest))
    return pairs


def integrity_matches(integrity, data):
    algorithm, _, encoded = integrity.partition("-")
    return algorithm == "sha512" and base64.b64encode(hashlib.sha512(data).digest()).decode() == encoded


def write_outputs(env, **values):
    with open(env["GITHUB_OUTPUT"], "a", encoding="utf-8") as outputs:
        for key, value in values.items():
            outputs.write(f"{key}={value}\n")


def resolve(env, root=Path(".")):
    metadata = json.loads(fetch(REGISTRY, 64 * 1024 * 1024))
    requested = env.get("REQUESTED_VERSION", "").strip()
    version = requested or metadata["dist-tags"]["latest"]
    if len(version) > 64 or not VERSION.fullmatch(version):
        fail(f"'{version[:80]}' is not a Codex release version")
    source = "input" if requested else "latest"
    if catalogued(json.loads((root / "qualified-builds.json").read_text()), version):
        print(f"codex {version} is already listed in qualified-builds.json")
        write_outputs(env, needed="false", version=version, source=source)
        return
    package = metadata["versions"].get(f"{version}-darwin-arm64")
    url = PACKAGE_URL.format(version=version)
    if package is None or package["dist"]["tarball"] != url:
        fail(f"npm has no darwin-arm64 package for codex {version}")
    data = fetch(url)
    if not integrity_matches(package["dist"]["integrity"], data):
        fail(f"{url} does not match its registry integrity")
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        member = archive.getmember(BINARY_MEMBER)
        if not member.isfile():
            fail(f"{BINARY_MEMBER} is not a regular file")
        binary = archive.extractfile(member).read()
    sha256 = hashlib.sha256(binary).hexdigest()
    if (version, sha256) in release_builds(root):
        print(f"codex {version} is handled by the release adapter; keep it out of the shared catalog")
        write_outputs(env, needed="false", version=version, sha256=sha256, source=source)
        return
    candidate = root / "candidate"
    candidate.mkdir(mode=0o700, exist_ok=True)
    (candidate / "codex").write_bytes(binary)
    (candidate / "codex").chmod(0o700)
    print(f"candidate codex {version} sha256 {sha256}")
    write_outputs(env, needed="true", version=version, sha256=sha256, package=url, source=source)


def load_verdict(directory):
    try:
        verdict = json.loads((Path(directory) / "verdict.json").read_text())
    except (OSError, ValueError):
        return None
    return verdict if isinstance(verdict, dict) and verdict.get("schema") == "xcb.codex-inventory-verdict.v1" else None


def consistent(verdict, status, version, sha256):
    return (verdict is not None and verdict.get("outcome") in EXIT_CODES
            and EXIT_CODES[verdict["outcome"]] == status
            and verdict.get("version") == version and verdict.get("binarySha256") == sha256)


CHECKS = [("emptyManifestVerified", "empty tool manifest"), ("exactDynamicManifestVerified", "exact dynamic tool manifest"),
          ("permittedCallbackVerified", "permitted host callback"), ("stdioJoined", "provider output closed"),
          ("listenerJoined", "loopback listener stopped")]


def code(value):
    # Values such as reasoning efforts come from the candidate's own bundled
    # catalog; keep them inert inside one code span.
    text = re.sub(r"[`\r\n]", "", str(value))[:80]
    return f"`{text}`"


def failed_case(entry):
    case = entry.get("case")
    if not isinstance(case, dict):
        return f"- {code(case)}"
    missing = [label for key, label in CHECKS if case.get(key) is False]
    if case.get("rootExitCode") not in (0, None):
        missing.append(f"exit code {code(case['rootExitCode'])}")
    detail = ", ".join(missing) or "see its evidence.json"
    rejected = code(case.get("forgedBuiltinRejections"))
    return f"- {code(case.get('model'))} at {code(case.get('reasoningEffort'))} effort: {detail} ({rejected} forged builtins rejected)"


def render(verdict, env):
    """Return (title, body) for one candidate's tracking issue."""
    version, sha256, outcome = env["VERSION"], env["SHA256"], verdict["outcome"]
    reason = verdict.get("reason")
    run_url, artifact, run_id = env["RUN_URL"], env["ARTIFACT"], env["RUN_ID"]
    lines = [
        f"<!-- xcb-codex-catalog version={version} outcome={outcome} -->",
        f"The Codex catalog check found Codex {version} on npm. `qualified-builds.json` does not list it, so xcb parks it and keeps running the pinned build.",
        "",
        f"- Package: [codex-{version}-darwin-arm64.tgz]({env['PACKAGE']}), registry integrity checked",
        f"- Binary SHA-256: `{sha256}`",
    ]
    if outcome == "passed":
        title = f"Codex {version} passed the scripted inventory: add it to qualified-builds.json"
        entry = {"version": version, "sha256": sha256, "platform": "darwin-aarch64", "qualifiedBy": f"codex-catalog-run-{run_id}"}
        lines += [
            f"- Inventory: passed {verdict.get('cases')} cases, evidence `{verdict.get('inventory')}` SHA-256 `{verdict.get('inventorySha256')}`",
            f"- Run: {run_url}, evidence artifact `{artifact}` (kept 30 days)",
            "",
            "## Next step",
            "",
            "Open a pull request that adds this entry at the top of the `codex` list in `qualified-builds.json`, and enable auto-merge. "
            f"In the same pull request, commit `inventory.json` from the artifact as `qualification/codex-{version}-inventory.json` so the evidence outlives the artifact.",
            "",
            "```json",
            json.dumps(entry, indent=2),
            "```",
            "",
            "This workflow does not open the pull request: GitHub Actions cannot create pull requests in this repository, "
            "and a pull request opened with `GITHUB_TOKEN` would not run the `Required` check.",
        ]
    elif outcome == "incompatible":
        baked = verdict.get("bakedVersion")
        if reason == "schema-drift":
            title = f"Codex {version} changes the app-server wire schema: needs an xcb release"
            lines.append(f"- App-server schema SHA-256: `{verdict.get('observedSchemaSha256')}`; xcb expects `{verdict.get('bakedSchemaSha256')}` (Codex {baked})")
            cause = "The Codex app-server wire schema changed. The catalog never admits a wire-protocol change."
        elif reason == "qualified-model":
            title = f"Codex {version} no longer bundles model {code(verdict.get('model'))}: needs an xcb release"
            cause = f"The bundled model catalog no longer lists {code(verdict.get('model'))}, which xcb routes to."
        else:
            title = f"Codex {version} bundles a model catalog xcb cannot read: needs an xcb release"
            cause = "The harness could not find exactly one bundled model catalog in the binary."
        lines += [
            f"- Run: {run_url}, evidence artifact `{artifact}` (kept 30 days)",
            "",
            "## Next step",
            "",
            f"{cause} Supporting this build takes an xcb release: update the Codex adapter and its `VERSION`, `BINARY_SHA256`, and "
            "`SCHEMA_SHA256` in `crates/xcb-runtime/src/codex/config.rs`, then run `qualification/codex-inventory.py` against the build "
            "and commit its evidence." + (f" Until then xcb keeps running the Codex builds already listed, such as {baked}." if baked else ""),
        ]
    else:
        failures = verdict.get("failures") or []
        title = f"Codex {version} failed the scripted inventory"
        lines += [
            f"- Inventory: {len(failures)} of {verdict.get('cases')} cases failed",
            f"- Run: {run_url}, evidence artifact `{artifact}` (kept 30 days)",
            "",
            "## Next step",
            "",
            "The catalog must not list this build. Failed cases:",
            "",
            *[failed_case(entry) for entry in failures[:20]],
            "",
            "Each case directory in the artifact has an `evidence.json` with the provider requests and violations. "
            "Adopting this build takes an xcb release that fixes the cause.",
        ]
    return title, "\n".join(lines) + "\n"


class GitHub:
    def __init__(self, repository, run=subprocess.run):
        self.repository, self.run = repository, run

    def api(self, *args, body=None):
        result = self.run(["gh", "api", *args] + (["--input", "-"] if body is not None else []),
                          input=None if body is None else json.dumps(body), capture_output=True, text=True, check=True)
        return json.loads(result.stdout) if result.stdout.strip() else None

    def catalog_issues(self):
        pages = self.api("--paginate", "--slurp", f"repos/{self.repository}/issues?state=open&per_page=100")
        issues = []
        for issue in [issue for page in pages for issue in page]:
            match = MARKER.search(issue.get("body") or "")
            if match and "pull_request" not in issue and (issue.get("user") or {}).get("login") == BOT:
                issues.append({"number": issue["number"], "title": issue["title"], "body": issue["body"], "version": match.group(1)})
        return issues

    def create(self, title, body):
        return self.api("-X", "POST", f"repos/{self.repository}/issues", body={"title": title, "body": body})["number"]

    def update(self, number, title, body):
        self.api("-X", "PATCH", f"repos/{self.repository}/issues/{number}", body={"title": title, "body": body})

    def close(self, number, comment, reason="completed"):
        self.api("-X", "POST", f"repos/{self.repository}/issues/{number}/comments", body={"body": comment})
        self.api("-X", "PATCH", f"repos/{self.repository}/issues/{number}", body={"state": "closed", "state_reason": reason})


def summarize(env, text):
    path = env.get("GITHUB_STEP_SUMMARY")
    if path:
        with open(path, "a", encoding="utf-8") as summary:
            summary.write(text + "\n")


def report(env, github, root=Path(".")):
    version = env["VERSION"]
    issues = github.catalog_issues()
    catalog = json.loads((root / "qualified-builds.json").read_text())
    release_owned = (version, env.get("SHA256")) in release_builds(root)
    if env["NEEDED"] == "false" or release_owned:
        if not release_owned and not catalogued(catalog, version):
            print("::error::the skipped candidate is neither catalogued nor an exact release-supported build")
            return 1
        for issue in issues:
            if catalogued(catalog, issue["version"]):
                github.close(issue["number"], f"Codex {issue['version']} is listed in `qualified-builds.json`. Closed by {env['RUN_URL']}.")
            elif release_owned and issue["version"] == version:
                github.close(issue["number"], f"This exact Codex {version} build is handled by xcb's release adapter. Keep it out of the shared catalog: older xcb releases cannot enforce the newer adapter's controls. Closed by {env['RUN_URL']}.")
        if release_owned:
            summarize(env, f"Codex {version} matches the release adapter's exact executable; no shared-catalog proposal is needed.")
        else:
            summarize(env, f"Codex {version} is listed in `qualified-builds.json`; nothing to do.")
        return 0
    if not DIGEST.fullmatch(env.get("SHA256", "")):
        print("::error::the candidate has no SHA-256")
        return 1
    verdict = load_verdict(env["INVENTORY_DIR"])
    status = int(env.get("INVENTORY_STATUS") or -1)
    if not consistent(verdict, status, version, env["SHA256"]):
        print(f"::error::the inventory stopped without a consistent verdict (exit status {status}); read the step log and the evidence artifact")
        return 1
    title, body = render(verdict, env)
    summarize(env, f"## {title}\n\n{body}")
    current = [issue for issue in issues if issue["version"] == version]
    if current:
        number = current[0]["number"]
        if (current[0]["title"], current[0]["body"]) != (title, body):
            github.update(number, title, body)
    else:
        number = github.create(title, body)
    if env.get("SOURCE") == "latest":
        for issue in issues:
            if issue["version"] != version:
                github.close(issue["number"], f"Superseded by #{number}: Codex {version} is the current npm release.", "not_planned")
    level = "notice" if verdict["outcome"] == "passed" else "warning"
    print(f"::{level}::{title} (#{number})")
    return 0


def main(argv, env):
    if argv[1:] == ["resolve"]:
        resolve(env)
        return 0
    if argv[1:] == ["report"]:
        return report(env, GitHub(env["GITHUB_REPOSITORY"]))
    print("usage: codex-catalog.py resolve|report", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv, os.environ))
