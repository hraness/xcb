import { nativePlatforms, type PublishedRelease } from "./publication";

const releasesUrl = "https://github.com/hraness/xcb/releases";

/** The platforms native release binaries are built for, as running text. */
export const nativePlatformSentence = "Native release binaries are built for macOS ARM64 (darwin-aarch64) and Linux x86_64 (linux-x86_64); other hosts build from source, and the updater fails closed anywhere else.";

/**
 * One truthful sentence about the latest verified release. Every public page
 * derives its release copy from the datum instead of a hand-written claim.
 */
export function ReleaseSummary({ release }: Readonly<{ release: PublishedRelease | null }>) {
  if (release === null) {
    return <p className="xcb-release-summary">No native release is published yet; install from source. <a href={releasesUrl}>Check release assets</a> for the first verified archive.</p>;
  }
  return <p className="xcb-release-summary">Latest verified release: <strong>v{release.version}</strong> · <a href={release.verificationRun}>public verification run</a> · <a href={`${releasesUrl}/tag/v${release.version}`}>release notes</a>.</p>;
}

/**
 * Per-platform native download links with their adjacent checksums, or the
 * honest source-install state when the datum carries no native assets.
 */
export function NativeDownloads({ release }: Readonly<{ release: PublishedRelease | null }>) {
  if (release === null || release.native.length === 0) {
    return (
      <div className="xcb-native-downloads">
        <ReleaseSummary release={release} />
        <p>{nativePlatformSentence}</p>
      </div>
    );
  }
  const assets = new Map(release.native.map((asset) => [asset.platform, asset]));
  return (
    <div className="xcb-native-downloads">
      <ReleaseSummary release={release} />
      <ul aria-label={`Native downloads for v${release.version}`}>
        {nativePlatforms.map(({ platform, label }) => {
          const asset = assets.get(platform);
          return (
            <li key={platform}>
              <span>{label}</span>
              {asset === undefined
                ? <span>Not built for this release; build from source.</span>
                : <span><a href={asset.url}>{`xcb-${release.version}-${platform}.tar.gz`}</a> · <a href={asset.sha256Url}>SHA-256</a></span>}
            </li>
          );
        })}
      </ul>
      <p>Verify the checksum before installing, or run <code>XCB_VERSION={release.version} ./scripts/install-native.sh</code> from a source checkout to fetch, verify, and install this exact release. Other hosts build from source.</p>
    </div>
  );
}

/** The compatibility package line, rendered only when the release carries that archive. */
export function CompatibilityArchive({ release }: Readonly<{ release: PublishedRelease | null }>) {
  if (release === null || release.archiveUrl === null) {
    return <p>No <code>@hraness/xcb</code> npm package is published{release === null ? "" : " for this release"}. Releases tagged v0.3.0 and earlier are AgentMixer package archives, not xcb.</p>;
  }
  return <p><a href={release.archiveUrl}>TypeScript compatibility archive</a> for v{release.version}. This is a separate surface from the native app; see the <a href="https://github.com/hraness/xcb/blob/main/docs/compatibility.md">compatibility reference</a>.</p>;
}
