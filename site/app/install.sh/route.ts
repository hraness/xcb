import { publishedRelease } from "../publication";
import { installScript } from "./install-script.generated";

/**
 * Serves https://xcb.sh/install.sh: scripts/install.sh with its one version
 * placeholder replaced by the verified release in site/published-release.json.
 * The site serves no binaries and cannot name a version the release record
 * does not carry.
 */
export const dynamic = "force-static";

const placeholder = "@XCB_RELEASE_VERSION@";

export function renderInstallScript(release = publishedRelease): string {
  if (release === null || release.native.length === 0) {
    return [
      "#!/bin/sh",
      "echo 'xcb install: no release is published yet; build from source: https://xcb.sh/install#source' >&2",
      "exit 1",
      "",
    ].join("\n");
  }
  if (installScript.split(placeholder).length !== 2) {
    throw new Error("scripts/install.sh must contain the release version placeholder exactly once.");
  }
  return installScript.replace(placeholder, release.version);
}

const body = renderInstallScript();

export function GET(): Response {
  return new Response(body, {
    headers: {
      // Plain text, so a browser shows the script for reading before you pipe it.
      "cache-control": "public, max-age=300",
      "content-type": "text/plain; charset=utf-8",
      "x-content-type-options": "nosniff",
    },
  });
}
