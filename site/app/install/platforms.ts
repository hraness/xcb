import type { PlatformBadge, PlatformInstallTarget } from "@hraness/design-kit/react";
import type { PublishedRelease } from "../publication";
import { installCommand, windowsInstallCommand } from "./commands";

function has(release: PublishedRelease, platform: string): boolean {
  return release.native.some((asset) => asset.platform === platform);
}

/**
 * The install tabs, macOS then Linux then Windows, derived from the native
 * archives the published release carries. A platform without an archive
 * shows why and what to do instead.
 */
export function installPlatforms(release: PublishedRelease): readonly PlatformInstallTarget[] {
  const linuxArm = has(release, "linux-aarch64");
  const linux = has(release, "linux-x86_64") || linuxArm;
  return [
    has(release, "darwin-aarch64")
      ? { id: "macos", command: installCommand, shell: "Terminal", note: "Apple silicon" }
      : { id: "macos", unavailable: true, unavailableNote: "No release for macOS yet. Build from source." },
    linux
      ? { id: "linux", command: installCommand, shell: "Terminal", note: `${has(release, "linux-x86_64") ? (linuxArm ? "x86_64 and ARM64" : "x86_64") : "ARM64"}, glibc 2.34+` }
      : { id: "linux", unavailable: true, unavailableNote: "No release for Linux yet. Build from source." },
    has(release, "windows-x86_64")
      ? { id: "windows", command: windowsInstallCommand, shell: "PowerShell", note: "x86_64 · providers run in WSL2" }
      : { id: "windows", unavailable: true, unavailableNote: "Runs in WSL2 with the Linux command.", command: installCommand, shell: "WSL2 terminal" },
  ];
}

/** The "Runs on" row: platforms with a native build in the published release. */
export function runsOnPlatforms(release: PublishedRelease | null): readonly (PlatformBadge | "macos" | "linux" | "windows")[] {
  if (release === null) return [];
  const windows: PlatformBadge = has(release, "windows-x86_64")
    ? { id: "windows", note: "providers via WSL2" }
    : { id: "windows", note: "via WSL2" };
  return [
    ...(has(release, "darwin-aarch64") ? ["macos" as const] : []),
    ...(has(release, "linux-x86_64") || has(release, "linux-aarch64") ? ["linux" as const] : []),
    ...(has(release, "linux-x86_64") || has(release, "windows-x86_64") ? [windows] : []),
  ];
}
