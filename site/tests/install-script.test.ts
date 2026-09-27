import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { renderInstallScript } from "../app/install.sh/route";
import { installScript } from "../app/install.sh/install-script.generated";
import { publishedRelease } from "../app/publication";

const repositoryScript = readFileSync(resolve(import.meta.dir, "../../scripts/install.sh"), "utf8");

describe("the /install.sh bootstrap installer", () => {
  test("serves the repository script, regenerated from scripts/install.sh", () => {
    expect(installScript).toBe(repositoryScript);
    expect(repositoryScript.split("@XCB_RELEASE_VERSION@")).toHaveLength(2);
  });

  test("installs the published release by default and never an unpublished one", () => {
    const served = renderInstallScript();
    expect(served).not.toContain("@XCB_RELEASE_VERSION@");
    if (publishedRelease !== null) {
      expect(served).toContain(`default_version="${publishedRelease.version}"`);
    }
    expect(served.startsWith("#!/bin/sh\n")).toBe(true);
    expect(renderInstallScript(null)).toContain("no release is published yet");
    expect(renderInstallScript(null)).toContain("exit 1");
  });

  test("downloads the tag-pinned installer over HTTPS and names the next step", () => {
    expect(repositoryScript).toContain("https://raw.githubusercontent.com/$repository/v$version/scripts/install-native.sh");
    expect(repositoryScript).toContain("--proto '=https'");
    expect(repositoryScript).toContain("Next: xcb setup claude");
    expect(repositoryScript).not.toMatch(/sudo/);
  });
});
