import { mkdir } from "node:fs/promises";
import { homedir } from "node:os";
import { isAbsolute, join, relative, sep } from "node:path";

import { boundedText } from "../validation.ts";
import { assertPrivateDirectory, canonicalizePrivatePath, PRIVATE_CONTROL_REJECT } from "../private-file.ts";

export const CLI_STATE_ENV = "XCB_STATE";
const STATE_DIRNAME = ".xcb";

/** Resolve the CLI state root without creating it. XCB_STATE wins when it
 * names an absolute path; otherwise the root is ~/.xcb. */
export function cliStateRootPath(env: (name: string) => string | undefined = (name) => process.env[name]): string {
  const override = env(CLI_STATE_ENV);
  if (override !== undefined) {
    return canonicalizePrivatePath(override, { code: "XCB_STATE_INVALID", reject: PRIVATE_CONTROL_REJECT, maxLength: Infinity });
  }
  const home = homedir();
  if (typeof home !== "string" || !isAbsolute(home)) throw new Error("XCB_HOME_UNAVAILABLE");
  return join(home, STATE_DIRNAME);
}

/** Open an existing physical directory owned by this user with mode 0700. */
export async function privateDirectory(path: string): Promise<string> {
  canonicalizePrivatePath(path, { code: "XCB_DIRECTORY_INVALID", reject: PRIVATE_CONTROL_REJECT, maxLength: Infinity });
  return (await assertPrivateDirectory(path, { code: "XCB_DIRECTORY_NOT_PRIVATE", owner: "self",
    mode: "ownerOnly", canonical: "self", statOrder: "realpathFirst", stats: "number" })).physical;
}

/** Create the state root and the named child, both physical and mode 0700. */
export async function ensureCliState(child?: string): Promise<{ root: string; path: string }> {
  const root = await mkdir(cliStateRootPath(), { mode: 0o700, recursive: true }).then(async () => await privateDirectory(cliStateRootPath()));
  if (child === undefined) return { root, path: root };
  const name = boundedText(child, 64);
  if (!/^[a-z][a-z0-9-]*$/u.test(name)) throw new Error("XCB_STATE_CHILD_INVALID");
  const path = join(root, name);
  await mkdir(path, { mode: 0o700, recursive: true });
  return { root, path: await privateDirectory(path) };
}

export function assertWorkspaceStateSeparation(workspace: string, stateRoot: string): void {
  const contains = (parent: string, child: string) => {
    const path = relative(parent, child);
    return path === "" || (path !== ".." && !path.startsWith(`..${sep}`) && !isAbsolute(path));
  };
  if (contains(workspace, stateRoot) || contains(stateRoot, workspace)) {
    throw new Error("workspace overlaps private xcb state; choose a separate workspace or state root");
  }
}
