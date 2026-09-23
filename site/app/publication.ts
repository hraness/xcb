import publication from "../published-release.json";

const releaseRoot = "https://github.com/hraness/xcb/releases/download/v";
const stableVersion = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/u;
const verificationRunPattern = /^https:\/\/github\.com\/hraness\/xcb\/actions\/runs\/[1-9][0-9]*$/u;

/** Platforms the release pipeline builds native binaries for, in display order. */
export const nativePlatforms = [
  { platform: "darwin-aarch64", label: "macOS ARM64 (Apple silicon)" },
  { platform: "linux-x86_64", label: "Linux x86_64" },
] as const;

export type NativePlatform = (typeof nativePlatforms)[number]["platform"];

export type NativeAsset = Readonly<{ platform: NativePlatform; url: string; sha256Url: string }>;

export type PublishedRelease = Readonly<{
  version: string;
  verificationRun: string;
  /** The TypeScript compatibility package archive, when the release carries one. */
  archiveUrl: string | null;
  /** Native binary archives, one per built platform, each with an adjacent checksum. */
  native: readonly NativeAsset[];
}>;

const platformNames = new Set<string>(nativePlatforms.map(({ platform }) => platform));

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseVersion(value: unknown): string {
  if (typeof value !== "string" || !stableVersion.test(value)
    || value.split(".").some((part) => BigInt(part) > BigInt(Number.MAX_SAFE_INTEGER))) {
    throw new TypeError("Published release version must be an exact stable version.");
  }
  return value;
}

function parseNativeAsset(value: unknown, version: string): NativeAsset {
  if (!isRecord(value) || Object.keys(value).sort().join(",") !== "platform,sha256Url,url") {
    throw new TypeError("Native release asset must carry exactly platform, url, and sha256Url.");
  }
  const { platform, url, sha256Url } = value;
  if (typeof platform !== "string" || !platformNames.has(platform)) {
    throw new TypeError("Native release asset names an unsupported platform.");
  }
  const expectedUrl = `${releaseRoot}${version}/xcb-${version}-${platform}.tar.gz`;
  if (url !== expectedUrl || sha256Url !== `${expectedUrl}.sha256`) {
    throw new TypeError("Native release asset must bind the exact xcb archive and checksum for its version and platform.");
  }
  return { platform: platform as NativePlatform, url, sha256Url };
}

export function parsePublishedRelease(value: unknown): PublishedRelease | null {
  if (!isRecord(value)) throw new TypeError("Published release must be an object.");
  if (Object.keys(value).sort().join(",") !== "archiveUrl,native,verificationRun,version") {
    throw new TypeError("Published release has unexpected fields.");
  }
  const { version, archiveUrl, verificationRun, native } = value;
  if (version === null && archiveUrl === null && verificationRun === null && native === null) return null;
  const parsedVersion = parseVersion(version);
  if (typeof verificationRun !== "string" || !verificationRunPattern.test(verificationRun)) {
    throw new TypeError("Published release must name its public verification run.");
  }
  if (archiveUrl !== null && archiveUrl !== `${releaseRoot}${parsedVersion}/hraness-xcb-${parsedVersion}.tgz`) {
    throw new TypeError("Published release must bind the exact xcb package archive to its version.");
  }
  if (!Array.isArray(native)) throw new TypeError("Published release native assets must be a list.");
  const assets = native.map((asset) => parseNativeAsset(asset, parsedVersion));
  if (new Set(assets.map((asset) => asset.platform)).size !== assets.length) {
    throw new TypeError("Published release lists a native platform more than once.");
  }
  if (archiveUrl === null && assets.length === 0) {
    throw new TypeError("Published release must carry at least one verified artifact.");
  }
  return { version: parsedVersion, verificationRun, archiveUrl: archiveUrl as string | null, native: assets };
}

/** Shared machine-readable release links, only for artifacts in the verified datum. */
export function publicationMarkdown(release: PublishedRelease | null): string {
  if (release === null) return "";
  const lines = [
    `Latest verified release: **v${release.version}** · [public verification run](${release.verificationRun}) · [release notes](https://github.com/hraness/xcb/releases/tag/v${release.version}).`,
    "",
  ];
  for (const { platform, label } of nativePlatforms) {
    const asset = release.native.find((entry) => entry.platform === platform);
    if (asset !== undefined) {
      lines.push(`- ${label}: [xcb-${release.version}-${platform}.tar.gz](${asset.url}) · [SHA-256](${asset.sha256Url})`);
    }
  }
  if (release.archiveUrl !== null) {
    lines.push(`- [TypeScript compatibility archive](${release.archiveUrl}) (separate from the native app)`);
  }
  return lines.join("\n");
}

export const publishedRelease = parsePublishedRelease(publication);
