import { describe, expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

import { LANDING_END, LANDING_START, readmeLanding, renderReadmeHtml, renderReadmeMarkdown } from "./readme-html.ts";
import { parsePublishedRelease } from "../app/publication.ts";
import { readmeWithPublication } from "./sync-readme.ts";

const repository = join(import.meta.dir, "..", "..");

test("adds the verified release line to both site README formats without download links", async () => {
  const source = await readFile(join(repository, "README.md"), "utf8");
  const release = parsePublishedRelease(JSON.parse(await readFile(join(repository, "site/tests/fixtures/published-release.json"), "utf8")));
  if (release === null) throw new Error("published fixture required");
  expect(readmeWithPublication(source, null)).toBe(source);
  const published = readmeWithPublication(source, release);
  expect(published.indexOf("Latest verified release:")).toBeGreaterThan(published.indexOf("### Install a verified release"));
  expect(published.indexOf("Latest verified release:")).toBeLessThan(published.indexOf("curl -fsSL https://xcb.sh/install.sh | sh"));
  expect(renderReadmeHtml(published)).toContain(`href="${release.verificationRun}"`);
  expect(renderReadmeMarkdown(published)).toContain(release.verificationRun);
  // xcb installs with one command; the site offers no archive downloads.
  for (const url of [release.archiveUrl!, ...release.native.flatMap((asset) => [asset.url, asset.sha256Url])]) {
    expect(renderReadmeHtml(published)).not.toContain(url);
    expect(renderReadmeMarkdown(published)).not.toContain(url);
  }
  expect(() => readmeWithPublication("# No installation heading", release)).toThrow("exactly one");
  expect(() => readmeWithPublication(`${source}\n### Install a verified release\n`, release)).toThrow("exactly one");
});

test("renders the repository README with stable heading fragments and repository-rooted relative links", async () => {
  const source = await readFile(join(repository, "README.md"), "utf8");
  const html = renderReadmeHtml(source);
  expect(html).toContain('<h3 id="install-a-verified-release">Install a verified release</h3>');
  expect(html).toContain('id="limits"');
  expect(html).toContain('href="https://github.com/hraness/xcb/blob/main/docs/compatibility.md"');
  expect(html).toContain('href="https://github.com/hraness/xcb/blob/main/docs/route.md"');
  expect(html).not.toContain("<script");
});

test("extracts the landing block between the shared Hraness markers", async () => {
  const source = await readFile(join(repository, "README.md"), "utf8");
  expect(source.indexOf(LANDING_START)).toBeGreaterThanOrEqual(0);
  expect(source.indexOf(LANDING_END)).toBeGreaterThan(source.indexOf(LANDING_START));
  const landing = readmeLanding(source);
  expect(landing.title).toBe("xcb");
  // The canonical one-line description leads the README.
  expect(landing.lead.startsWith("xcb routes coding tasks across the Claude, Codex, and Devin subscriptions you already pay for.")).toBe(true);
  // The README never types the current version; the site inserts the verified release.
  const { version } = JSON.parse(await readFile(join(repository, "package.json"), "utf8")) as { version: string };
  const escapedVersion = version.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
  // A provider version may contain the xcb version as a numeric substring.
  // A sentence-ending period still belongs outside the release version.
  const releaseVersion = new RegExp(`(?<![0-9.])${escapedVersion}(?![0-9]|\\.[0-9])`, "u");
  expect(`xcb v${version}.`).toMatch(releaseVersion);
  expect(`npm install @hraness/xcb@${version}`).toMatch(releaseVersion);
  expect(`provider 300${version}1`).not.toMatch(releaseVersion);
  expect(`provider 1.${version}`).not.toMatch(releaseVersion);
  expect(`provider ${version}.1`).not.toMatch(releaseVersion);
  expect(source).not.toMatch(releaseVersion);
  expect(source).toContain("https://github.com/hraness/xcb/releases/latest");
});

test("serves the README as markdown with repository-rooted relative links", async () => {
  const source = await readFile(join(repository, "README.md"), "utf8");
  const markdown = renderReadmeMarkdown(source);
  expect(markdown).toContain("# xcb");
  expect(markdown).not.toContain("hraness:xcb-landing");
  expect(markdown).toContain("](https://github.com/hraness/xcb/blob/main/docs/compatibility.md)");
  expect(markdown).toContain("](https://github.com/hraness/xcb/blob/main/LICENSE)");
  expect(markdown).toContain("](https://xcb.sh)");
  expect(() => renderReadmeMarkdown("[x](javascript:alert(1))")).toThrow();
  expect(() => renderReadmeMarkdown("[x](//evil.example)")).toThrow();
});

test("rejects unsafe README link targets", () => {
  expect(() => renderReadmeHtml("[x](javascript:alert(1))")).toThrow("disallowed URL scheme");
  expect(() => renderReadmeHtml("[x](//evil.example)")).toThrow("protocol-relative");
  expect(() => renderReadmeHtml("[x](#missing)")).toThrow("no rendered heading");
});


test("omits repository landing markers", async () => {
  const source = await Bun.file(new URL("../../README.md", import.meta.url)).text();
  const html = renderReadmeHtml(source);
  expect(html).not.toContain("hraness:xcb-landing");
});


describe("README HTML boundary", () => {
  test("derives stable fragments from parsed heading text", () => {
    const html = renderReadmeHtml([
      "## **Hello** &amp; `world`",
      "## **Hello** &amp; `world`",
      "[First](#hello--world) [Again](#hello--world-1)",
    ].join("\n\n"));
    expect(html).toContain('<h2 id="hello--world"><strong>Hello</strong> &amp; <code>world</code></h2>');
    expect(html).toContain('<h2 id="hello--world-1">');
  });

  test("keeps raw and nested malformed HTML inert, including inside headings", () => {
    const payloads = [
      '<script>alert(1)</script>',
      '<sc<script>ript>alert(1)</sc</script>ript>',
      '<img src="x" onerror="alert(1)">',
      '<svg onload="alert(1)"><a href="javascript:alert(1)">x</a></svg>',
      '<textarea><img src=x onerror=alert(1)></textarea>',
    ];
    for (const payload of payloads) {
      const html = renderReadmeHtml(`## Literal ${payload}\n\n${payload}`);
      const elements: string[] = [];
      const ids: string[] = [];
      new HTMLRewriter().on("*", {
        element(element) {
          elements.push(element.tagName);
          for (const [name] of element.attributes) expect(name).not.toMatch(/^on/iu);
          const id = element.getAttribute("id");
          if (id !== null) ids.push(id);
        },
      }).transform(html);
      expect(elements).toEqual(["h2", "p"]);
      expect(ids).toHaveLength(1);
      expect(ids[0]).toMatch(/^[\p{Letter}\p{Mark}\p{Number}_-]+$/u);
      expect(html).toContain("&lt;");
    }
  });

  test("rejects executable and protocol-relative Markdown URLs", () => {
    for (const target of ["javascript:alert", "java&#x73;cript:alert", "data:text/html,bad", "//example.com"]) {
      expect(() => renderReadmeHtml(`[link](${target})`)).toThrow();
      expect(() => renderReadmeHtml(`![image](${target})`)).toThrow();
    }
    expect(renderReadmeHtml("[Reference](docs/example.md)")).toContain(
      'href="https://github.com/hraness/xcb/blob/main/docs/example.md"',
    );
  });
});
