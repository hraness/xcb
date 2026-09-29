import { publishedRelease } from "../publication";
import { installScript } from "./install-script.generated";

const placeholder = "@XCB_RELEASE_VERSION@";

/**
 * scripts/install.ps1 with its one version placeholder replaced by the
 * verified release in site/published-release.json, once that release carries
 * a Windows build. The site serves no binaries and cannot name a version the
 * release record does not carry. The refusal throws rather than exits, so
 * `irm | iex` never closes the caller's PowerShell window.
 */
export function renderWindowsInstallScript(release = publishedRelease): string {
  if (release === null || !release.native.some(({ platform }) => platform === "windows-x86_64")) {
    return [
      "throw 'xcb install: no Windows release is published yet; see https://xcb.sh/install'",
      "",
    ].join("\n");
  }
  if (installScript.split(placeholder).length !== 2) {
    throw new Error("scripts/install.ps1 must contain the release version placeholder exactly once.");
  }
  return installScript.replace(placeholder, release.version);
}
