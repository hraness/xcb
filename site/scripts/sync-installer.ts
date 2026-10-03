import { resolve } from "node:path";

const siteRoot = resolve(import.meta.dir, "..");
const repositoryRoot = resolve(siteRoot, "..");

/** The one placeholder the served script replaces with the published version. */
export const releasePlaceholder = "@XCB_RELEASE_VERSION@";

/** Each served installer: the repository script and the route folder it is copied into. */
const installers = [
  { script: "scripts/install.sh", route: "app/install.sh" },
  { script: "scripts/install.ps1", route: "app/install.ps1" },
] as const;

if (import.meta.main) {
  for (const { script: path, route } of installers) {
    const script = await Bun.file(resolve(repositoryRoot, path)).text();
    if (script.split(releasePlaceholder).length !== 2) {
      throw new Error(`${path} must contain the release version placeholder exactly once.`);
    }
    await Bun.write(
      resolve(siteRoot, `${route}/install-script.generated.ts`),
      `// Generated from ../${path} by scripts/sync-installer.ts. Do not edit.\n`
        + `export const installScript = ${JSON.stringify(script)};\n`,
    );
  }
}
