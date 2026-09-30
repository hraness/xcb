import { describe, expect, test } from "bun:test";
import { compatibilityUpdateOptions } from "../src/cli/update.ts";

const entrypoint = "/synthetic/global/node_modules/@hraness/xcb/dist/cli.js";
const options = (argv: readonly string[], env: NodeJS.ProcessEnv = {}, stdinIsTTY = true, stderrIsTTY = true) =>
  compatibilityUpdateOptions({ version: "1.2.3", entrypoint, argv, env, stdinIsTTY, stderrIsTTY });

describe("compatibility CLI update policy", () => {
  test("binds the actual executable to the existing immutable xcb archive channel", () => {
    const configured = options(["update", "check", "--json"]);
    expect(configured.packageName).toBe("@hraness/xcb");
    expect(configured.binName).toBe("xcb-compat");
    expect(configured.entrypoint).toBe(entrypoint);
    expect(configured.provider).toEqual({
      kind: "github", repository: "hraness/xcb", assetName: "hraness-xcb-{version}.tgz", channel: "stable",
    });
    expect(configured.ignoreScripts).toBe(true);
  });

  test("permits updates before interactive work and keeps maintenance commands unchanged", () => {
    for (const argv of [[], ["chat"], ["run", "-p", "task"], ["resume"], ["doctor"], ["./workspace"], ["--provider", "claude"]]) {
      expect(options(argv).suppressAutomatic).toBe(false);
    }
    for (const command of ["auth", "sessions", "judge", "migrate", "update"]) {
      expect(options([command]).suppressAutomatic).toBe(true);
    }
    expect(options(["run"], {}, false, true).suppressAutomatic).toBe(true);
    expect(options(["run"], {}, true, false).suppressAutomatic).toBe(true);
    expect(options(["run", "--json"]).suppressAutomatic).toBe(true);
  });

  test("honors the native opt-outs, audience, product pin, and reentry guards", () => {
    for (const key of ["HRANESS_NO_UPDATE", "XCB_NO_UPDATE", "CI"]) {
      expect(options([], { [key]: "1" }).suppressAutomatic).toBe(true);
      expect(options([], { [key]: "true" }).suppressAutomatic).toBe(true);
      expect(options([], { [key]: "false" }).suppressAutomatic).toBe(false);
      expect(options([], { [key]: "0" }).suppressAutomatic).toBe(false);
    }
    expect(options([], { HRANESS_AUDIENCE: "agent" }).suppressAutomatic).toBe(true);
    expect(options([], { HRANESS_AUDIENCE: "human" }).suppressAutomatic).toBe(false);
    expect(options([], { XCB_VERSION: "1.2.3" }).pinned).toBe(true);
    expect(options([], { XCB_VERSION: "" }).pinned).toBe(true);
    expect(options([], { XCB_UPDATE_REENTRY: "1" }).nested).toBe(true);
    expect(options([], { HRANESS_UPDATE_REENTRY: "1" }).nested).toBe(true);
  });

  test("only parser-confirmed help/version paths bypass update coordination", () => {
    for (const command of ["--help", "-h", "help", "--version", "-v"]) {
      expect(options([command, "ignored-by-existing-parser"]).effectFree).toBe(true);
    }
    expect(options(["run", "-p", "--help"]).effectFree).toBe(false);
    expect(options(["doctor"]).effectFree).toBe(false);
    // These are workspace names, not readonly aliases in the product parser.
    for (const workspace of ["version", "completion", "completions"]) {
      expect(options([workspace]).effectFree).toBe(false);
    }
  });
});
