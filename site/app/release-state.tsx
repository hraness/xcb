import type { PublishedRelease } from "./publication";
import sourcePackage from "../../package.json";

const releasesUrl = "https://github.com/hraness/xcb/releases";

/**
 * The page's one status label (STYLE.md: state the status once, with one of
 * the allowed labels). Every page and the blog render it from the datum.
 */
export function releaseStatusLabel(release: PublishedRelease | null): string {
  return release === null ? "Preview: install from source" : `Latest release: v${release.version}`;
}

/** The platforms the release pipeline builds native archives for, as running text. */
export const nativePlatformSentence = "The release pipeline builds native archives for macOS ARM64 (darwin-aarch64) and Linux x86_64 (linux-x86_64). Other hosts build from source, and the updater installs nothing on them.";

/**
 * One truthful sentence about the latest verified release. Every public page
 * derives its release copy from the datum instead of a hand-written claim.
 */
export function ReleaseSummary({ release }: Readonly<{ release: PublishedRelease | null }>) {
  if (release === null) {
    return <p className="xcb-release-summary">No native release is published yet; install from source. <a href={releasesUrl}>Check release assets</a> for the first verified archive.</p>;
  }
  return <p className="xcb-release-summary">Latest verified release: <strong>v{release.version}</strong> · <a href={release.verificationRun}>public verification run</a> · <a href={`${releasesUrl}/tag/v${release.version}`}>release notes</a>.{release.version !== sourcePackage.version && <> Documentation follows source v{sourcePackage.version}; unreleased commands require a source build.</>}</p>;
}
