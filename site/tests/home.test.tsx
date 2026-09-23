import { expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { renderToStaticMarkup } from "react-dom/server";
import Home from "../app/page";
import Docs from "../app/docs/page";
import Compare from "../app/compare/page";
import { parsePublishedRelease, publishedRelease } from "../app/publication";
import { CompatibilityArchive, NativeDownloads } from "../app/release-state";
import RootLayout from "../app/layout";
import { siteDefaultPalette } from "../palette";

test("the appearance menu agrees with the bootstrap Paper/system preference", () => {
  const html = renderToStaticMarkup(<RootLayout><Home /></RootLayout>);
  const selected: string[] = [];
  new HTMLRewriter().on('.hraness-design-palette-menu input[type="radio"][checked]', {
    element(element) { selected.push(element.getAttribute("value") ?? ""); },
  }).transform(html);
  expect(siteDefaultPalette).toEqual({ palette: "paper", mode: "system" });
  expect(selected).toEqual([siteDefaultPalette.palette, siteDefaultPalette.mode]);
});

test("public entry pages keep one optional support footer and no product signup", () => {
  for (const Page of [Home, Docs]) {
    const html = renderToStaticMarkup(<RootLayout><Page /></RootLayout>);
    expect(html.match(/<footer\b/gu)).toHaveLength(1);
    expect(html).toContain("https://account.hraness.com/support?product=xcb&amp;source=web#support");
    expect(html).not.toContain('type="email"');
    expect(html).not.toContain("source=web#updates");
  }
});

test("the homepage clearly separates source installation from verified releases", () => {
  const html = renderToStaticMarkup(<Home />);
  expect(html.match(/<h1\b/gu)).toHaveLength(1);
  expect(html).toContain("One terminal. Your coding agents.");
  expect(html).toContain("./scripts/install-native.sh");
  if (publishedRelease === null) {
    expect(html).toContain("No native release is published yet; install from source");
    expect(html).not.toContain(".tgz");
    expect(html).not.toContain("npm install @hraness/xcb");
  } else {
    expect(html).toContain(`v${publishedRelease.version}`);
    expect(html).toContain(publishedRelease.verificationRun);
    for (const asset of publishedRelease.native) expect(html).toContain(asset.url);
    if (publishedRelease.archiveUrl !== null) expect(html).toContain(publishedRelease.archiveUrl);
  }
});

test("the release state renders verified native downloads from the fixture datum", async () => {
  const fixture = JSON.parse(
    await readFile(join(import.meta.dir, "fixtures/published-release.json"), "utf8"),
  ) as unknown;
  const release = parsePublishedRelease(fixture);
  if (release === null) throw new Error("The release fixture must exercise the published state.");
  const downloads = renderToStaticMarkup(<NativeDownloads release={release} />);
  expect(downloads).toContain("Latest verified release:");
  expect(downloads).toContain("<strong>v0.20.0</strong>");
  for (const asset of release.native) {
    expect(downloads).toContain(`href="${asset.url}"`);
    expect(downloads).toContain(`href="${asset.sha256Url}"`);
  }
  expect(downloads).toContain(release.verificationRun);
  if (release.archiveUrl === null) throw new Error("The release fixture must carry the compatibility archive.");
  const archive = renderToStaticMarkup(<CompatibilityArchive release={release} />);
  expect(archive).toContain(`href="${release.archiveUrl}"`);
});

test("the product example is illustrative, with accessible working view controls", () => {
  const html = renderToStaticMarkup(<Home />);
  const buttons: string[] = [];
  new HTMLRewriter().on('button[aria-controls="workspace-example"]', {
    element(element) { buttons.push(element.getAttribute("aria-pressed") ?? ""); },
  }).transform(html);
  expect(buttons).toEqual(["true", "false", "false"]);
  expect(html).toContain("Illustrative workspace");
  expect(html).toContain("no live provider calls");
  expect(html).toContain('aria-live="polite"');
  expect(html).not.toContain("Subagents");
});

test("support boundaries appear before installation without implying offline inference", () => {
  const html = renderToStaticMarkup(<Home />);
  expect(html).toContain("source preview");
  expect(html).toContain("macOS ARM64");
  expect(html).toContain("still needs its own evidence");
  expect(html).toContain("Offline Linux ARM64");
  expect(html).toContain("No native macOS command execution");
  expect(html).toContain("Model requests still go to the provider");
  expect(html.indexOf('id="readiness"')).toBeLessThan(html.indexOf('id="install"'));
  expect(html).toContain('href="/compare"');
  expect(html).toContain('href="/docs/getting-started"');
});


test("the shared header keeps a named home link and exact-artwork foil fallback", () => {
  for (const Page of [Home, Docs, Compare]) {
    const html = renderToStaticMarkup(<Page />);
    const homeLinks: string[] = [];
    const marks: string[] = [];
    const fallbackImages: string[] = [];
    const masks: string[] = [];
    new HTMLRewriter()
      .on('header a[aria-label="xcb home"]', {
        element(element) {
          homeLinks.push(element.getAttribute("href") ?? "");
          expect(element.hasAttribute("data-foil")).toBe(true);
        },
      })
      .on('header a[aria-label="xcb home"] .hraness-foil-mark', {
        element(element) { marks.push(element.getAttribute("aria-hidden") ?? ""); },
      })
      .on('header a[aria-label="xcb home"] .hraness-foil-mark img', {
        element(element) {
          fallbackImages.push(element.getAttribute("src") ?? "");
          expect(element.hasAttribute("alt")).toBe(true);
          expect(element.getAttribute("alt") ?? "").toBe("");
        },
      })
      .on('header a[aria-label="xcb home"] .hraness-foil-mark__paint', {
        element(element) { masks.push((element.getAttribute("style") ?? "").replaceAll("&quot;", '"')); },
      })
      .transform(html);
    expect(homeLinks).toEqual(["/"]);
    expect(marks).toEqual(["true"]);
    expect(fallbackImages).toEqual(["/marks/xcb.svg"]);
    expect(masks).toEqual(['--hraness-foil-mask:url("/marks/xcb.svg")']);
  }
});
