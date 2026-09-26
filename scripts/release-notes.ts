import {
  releaseBody,
  releaseDistribution,
  releaseIdentityRecord,
  type ReleasePackage,
} from "./release-distribution-policy.ts";

const SEMVER = /^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)$/u;
const COMMIT = /^[0-9a-f]{40}$/u;
const MAXIMUM_CHANGELOG_BYTES = 1_024 * 1_024;

export type ChangelogSection = Readonly<{ changes: string; summary: string }>;

/** Read one version's section from CHANGELOG.md. The heading is `## X.Y.Z` or
 * `## vX.Y.Z`, optionally followed by ` - YYYY-MM-DD`. The section holds a
 * summary paragraph and then a bulleted list, and ends at the next `## ` or
 * `# ` heading. A missing, duplicated, or empty section, a section without a
 * summary or bullets, and a section that still says Unreleased all fail. */
export function changelogSection(changelog: string, version: string): ChangelogSection {
  if (!SEMVER.test(version)) throw new Error("Changelog version is invalid.");
  if (changelog.length > MAXIMUM_CHANGELOG_BYTES) throw new Error("CHANGELOG.md exceeds its size bound.");
  if (changelog.includes("\r")) throw new Error("CHANGELOG.md must use LF line endings.");
  const lines = changelog.split("\n");
  const escaped = version.replaceAll(".", "\\.");
  const heading = new RegExp(`^## v?${escaped}(?: - [0-9]{4}-[0-9]{2}-[0-9]{2})?$`, "u");
  const starts = lines.flatMap((line, index) => (heading.test(line) ? [index] : []));
  if (starts.length === 0) throw new Error(`CHANGELOG.md has no section for ${version}.`);
  if (starts.length > 1) throw new Error(`CHANGELOG.md has more than one section for ${version}.`);
  const start = starts[0]! + 1;
  let end = lines.length;
  for (let index = start; index < lines.length; index += 1) {
    if (/^#{1,2} /u.test(lines[index]!)) {
      end = index;
      break;
    }
  }
  const body = lines.slice(start, end).join("\n").trim();
  if (body.length === 0) throw new Error(`CHANGELOG.md section ${version} is empty.`);
  if (/\bunreleased\b/iu.test(body)) throw new Error(`CHANGELOG.md section ${version} still says Unreleased.`);
  const bodyLines = body.split("\n");
  const firstBullet = bodyLines.findIndex((line) => line.startsWith("- "));
  if (firstBullet < 0) throw new Error(`CHANGELOG.md section ${version} has no bulleted changes.`);
  const summary = bodyLines.slice(0, firstBullet).join("\n").trim();
  if (summary.length === 0) throw new Error(`CHANGELOG.md section ${version} has no summary paragraph.`);
  const changes = bodyLines.slice(firstBullet).join("\n").trim();
  return Object.freeze({ changes, summary });
}

export type ReleaseNotesInput = Readonly<{
  changelog: string;
  commit: string;
  releasePackage: ReleasePackage;
  version: string;
}>;

/** Render the human-readable part of a GitHub Release page: the changelog
 * summary, `## Changes`, and the generated `## Install` and `## Verify`
 * sections. Every name, version, and URL comes from the release record. */
export function renderReleaseNotes(input: ReleaseNotesInput): string {
  const { commit, releasePackage, version } = input;
  if (!COMMIT.test(commit)) throw new Error("Release notes need one full source commit.");
  const section = changelogSection(input.changelog, version);
  const distribution = releaseDistribution(releasePackage);
  const tag = `${releasePackage.tagPrefix}${version}`;
  const repository = releasePackage.repository;
  const archive = distribution.releaseArchiveName(version);
  const download = `https://github.com/${repository}/releases/download/${tag}`;
  const native = `xcb-${version}-<platform>.tar.gz`;
  return [
    section.summary,
    "## Changes",
    section.changes,
    "## Install",
    "Install the native `xcb` binary from this release. The installer downloads the archive for your platform, checks it against its `.sha256` file, and installs `~/.local/bin/xcb`:",
    [
      "```sh",
      `git clone --depth 1 --branch ${tag} https://github.com/${repository}.git xcb`,
      `cd xcb && XCB_VERSION=${version} ./scripts/install-native.sh`,
      "```",
    ].join("\n"),
    "Install the TypeScript SDK and `xcb-compat` CLI from the release tarball:",
    ["```sh", `npm install ${download}/${archive}`, "```"].join("\n"),
    "## Verify",
    [
      `- Checksums: \`SHA256SUMS\` lists the SHA-256 of \`${archive}\`, and each \`${native}\` has its own \`.sha256\` file.`,
      `- Source commit: \`${commit}\``,
      `- Build provenance: \`gh attestation verify ${native} -R ${repository}\`. The [publishing guide](https://github.com/${repository}/blob/${tag}/docs/publishing.md) describes how each file is built and checked.`,
    ].join("\n"),
  ].join("\n\n");
}

/** The full release body: rendered notes, then the identity record as the
 * trailing HTML comment that forms the final bytes. */
export function renderReleaseBody(input: ReleaseNotesInput): string {
  const tag = `${input.releasePackage.tagPrefix}${input.version}`;
  return releaseBody(
    renderReleaseNotes(input),
    releaseIdentityRecord(input.releasePackage, input.version, tag),
  );
}

/** The release page title: the registry product name, a space, and the tag. */
export function releaseTitle(releasePackage: ReleasePackage, version: string): string {
  if (!SEMVER.test(version)) throw new Error("Release title version is invalid.");
  return `${releasePackage.title} ${releasePackage.tagPrefix}${version}`;
}

if (import.meta.main) {
  // Manual path: print the standard body for one version, for
  // `gh release edit <tag> --notes-file`.
  const [versionArgument, commitArgument, changelogArgument, extra] = process.argv.slice(2);
  if (versionArgument === undefined || commitArgument === undefined || extra !== undefined) {
    throw new Error("Usage: release-notes.ts VERSION COMMIT [CHANGELOG.md]");
  }
  const { readFileSync } = await import("node:fs");
  const { resolve } = await import("node:path");
  const { rootReleasePackage } = await import("./release-distribution-policy.ts");
  const changelog = readFileSync(
    resolve(changelogArgument ?? resolve(import.meta.dir, "..", "CHANGELOG.md")),
    "utf8",
  );
  process.stdout.write(renderReleaseBody({
    changelog,
    commit: commitArgument,
    releasePackage: rootReleasePackage,
    version: versionArgument,
  }));
}
