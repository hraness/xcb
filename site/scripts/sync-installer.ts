import { resolve } from "node:path";

const siteRoot = resolve(import.meta.dir, "..");
const repositoryRoot = resolve(siteRoot, "..");

/** The one placeholder the served script replaces with the published version. */
export const releasePlaceholder = "@XCB_RELEASE_VERSION@";

if (import.meta.main) {
  const script = await Bun.file(resolve(repositoryRoot, "scripts/install.sh")).text();
  if (script.split(releasePlaceholder).length !== 2) {
    throw new Error("scripts/install.sh must contain the release version placeholder exactly once.");
  }
  await Bun.write(
    resolve(siteRoot, "app/install.sh/install-script.generated.ts"),
    "// Generated from ../scripts/install.sh by scripts/sync-installer.ts. Do not edit.\n"
      + `export const installScript = ${JSON.stringify(script)};\n`,
  );
}
