import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { renderInstallScript } from "../app/install.sh/render";
import { installScript } from "../app/install.sh/install-script.generated";
import { renderWindowsInstallScript } from "../app/install.ps1/render";
import { installScript as windowsInstallScript } from "../app/install.ps1/install-script.generated";
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

describe("the /install.ps1 Windows installer", () => {
  const repositoryWindowsScript = readFileSync(resolve(import.meta.dir, "../../scripts/install.ps1"), "utf8");

  test("serves the repository script, regenerated from scripts/install.ps1", () => {
    expect(windowsInstallScript).toBe(repositoryWindowsScript);
    expect(repositoryWindowsScript.split("@XCB_RELEASE_VERSION@")).toHaveLength(2);
  });

  test("installs only a published release that carries a Windows build", () => {
    expect(renderWindowsInstallScript(null)).toContain("no Windows release is published yet");
    expect(renderWindowsInstallScript(null)).not.toContain("exit");
    if (publishedRelease !== null) {
      const withoutWindows = { ...publishedRelease, native: publishedRelease.native.filter(({ platform }) => platform !== "windows-x86_64") };
      expect(renderWindowsInstallScript(withoutWindows)).toContain("no Windows release is published yet");
      const version = publishedRelease.version;
      const withWindows = {
        ...withoutWindows,
        native: [...withoutWindows.native, {
          platform: "windows-x86_64" as const,
          url: `https://github.com/hraness/xcb/releases/download/v${version}/xcb-${version}-windows-x86_64.zip`,
          sha256Url: `https://github.com/hraness/xcb/releases/download/v${version}/xcb-${version}-windows-x86_64.zip.sha256`,
        }],
      };
      const served = renderWindowsInstallScript(withWindows);
      expect(served).not.toContain("@XCB_RELEASE_VERSION@");
      expect(served).toContain(`$defaultVersion = '${version}'`);
    }
  });

  test("verifies the checksum, admits only xcb.exe, and never elevates", () => {
    expect(repositoryWindowsScript).toContain("Get-FileHash");
    expect(repositoryWindowsScript).toContain("archive must contain only xcb.exe");
    expect(repositoryWindowsScript).toContain("WSL2");
    expect(repositoryWindowsScript).not.toMatch(/RunAs|Start-Process/);
  });
});
