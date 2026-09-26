import { describe, expect, test } from "bun:test";

import {
  parseGitHubRelease,
  releaseIdentityRecord,
  rootReleasePackage,
  splitReleaseBody,
} from "./release-distribution-policy";
import {
  changelogSection,
  releaseTitle,
  renderReleaseBody,
  renderReleaseNotes,
} from "./release-notes";

const commit = "65a9bcc4fe2ccfe0aa7d7bb29b10e40dcf0868f0";
const changelog = [
  "# Changelog",
  "",
  "Intro text.",
  "",
  "## Unreleased",
  "",
  "Work in progress.",
  "",
  "- Not shipped yet.",
  "",
  "## v1.2.3 - 2026-09-26",
  "",
  "The summary of 1.2.3.",
  "",
  "- First change.",
  "- Second change",
  "  continues here.",
  "",
  "## 1.2.2",
  "",
  "- Old change without a summary.",
  "",
].join("\n");
const input = { changelog, commit, releasePackage: rootReleasePackage, version: "1.2.3" } as const;

describe("release notes from CHANGELOG.md", () => {
  test("copies the version's summary and bullets and ignores other sections", () => {
    expect(changelogSection(changelog, "1.2.3")).toEqual({
      changes: "- First change.\n- Second change\n  continues here.",
      summary: "The summary of 1.2.3.",
    });
  });

  test("fails when the section is missing, empty, incomplete, duplicated, or Unreleased", () => {
    expect(() => changelogSection(changelog, "9.9.9")).toThrow("no section for 9.9.9");
    expect(() => changelogSection("## 1.0.0\n\n## 0.9.0\n\nOld.\n\n- Old.\n", "1.0.0")).toThrow("is empty");
    expect(() => changelogSection(changelog, "1.2.2")).toThrow("no summary paragraph");
    expect(() => changelogSection("## 1.0.0\n\nSummary only.\n", "1.0.0")).toThrow("no bulleted changes");
    expect(() => changelogSection("## 1.0.0\n\nUnreleased.\n\n- Change.\n", "1.0.0")).toThrow("Unreleased");
    expect(() => changelogSection("## 1.0.0\n\nA.\n\n- B.\n\n## v1.0.0\n\nC.\n\n- D.\n", "1.0.0"))
      .toThrow("more than one section");
    expect(() => changelogSection("## 1.0.0\r\n\r\nA.\r\n\r\n- B.\r\n", "1.0.0")).toThrow("LF");
    expect(() => changelogSection("## 1.0.0-rc.1\n\nA.\n\n- B.\n", "1.0.0")).toThrow("no section");
    expect(() => changelogSection(changelog, "latest")).toThrow("version is invalid");
    expect(() => renderReleaseNotes({ ...input, commit: "abc" })).toThrow("full source commit");
  });

  test("renders the standard page shape with the identity record as the final bytes", () => {
    const body = renderReleaseBody(input);
    const notes = renderReleaseNotes(input);
    const headings = body.split("\n").filter((line) => line.startsWith("## "));
    expect(headings).toEqual(["## Changes", "## Install", "## Verify"]);
    expect(body.startsWith("The summary of 1.2.3.\n\n## Changes\n\n- First change.")).toBe(true);
    expect(body).toContain(
      "npm install https://github.com/hraness/xcb/releases/download/v1.2.3/hraness-xcb-1.2.3.tgz",
    );
    expect(body).toContain("git clone --depth 1 --branch v1.2.3 https://github.com/hraness/xcb.git xcb");
    expect(body).toContain("XCB_VERSION=1.2.3 ./scripts/install-native.sh");
    expect(body).toContain(`Source commit: \`${commit}\``);
    expect(body).toContain("https://github.com/hraness/xcb/blob/v1.2.3/docs/publishing.md");
    expect(body).not.toMatch(/latest|What's Changed|Full Changelog|Generated with|Canonical GitHub release for/u);
    expect(notes).not.toContain("Automated");
    const identity = releaseIdentityRecord(rootReleasePackage, "1.2.3", "v1.2.3");
    expect(identity).toBe("<!-- Automated public release of @hraness/xcb@1.2.3 from v1.2.3. -->");
    expect(body).toBe(`${notes}\n\n${identity}`);
    expect(body.endsWith("-->")).toBe(true);
    expect(releaseTitle(rootReleasePackage, "1.2.3")).toBe("xcb v1.2.3");
  });

  test("the identity still parses and tampered notes are detected", () => {
    const body = renderReleaseBody(input);
    const notes = renderReleaseNotes(input);
    expect(splitReleaseBody(body)).toEqual({
      identity: releaseIdentityRecord(rootReleasePackage, "1.2.3", "v1.2.3"),
      notes,
    });
    const payload = (overrides: Readonly<Record<string, unknown>>) => ({
      assets: [],
      body,
      draft: false,
      immutable: true,
      name: "xcb v1.2.3",
      prerelease: false,
      tag_name: "v1.2.3",
      ...overrides,
    });
    // Body checks pass; the empty asset list is the next failure.
    expect(() => parseGitHubRelease(payload({}), "1.2.3", notes)).toThrow("two exact release artifacts");
    const tampered = body.replace("- First change.", "- First change, edited by hand.");
    expect(() => parseGitHubRelease(payload({ body: tampered }), "1.2.3", notes)).toThrow("notes do not match");
    const otherCommit = renderReleaseNotes({ ...input, commit: "0".repeat(40) });
    expect(() => parseGitHubRelease(payload({}), "1.2.3", otherCommit)).toThrow("notes do not match");
  });
});
