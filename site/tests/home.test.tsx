import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import Home from "../app/page";
import Docs from "../app/docs/page";
import { publishedRelease } from "../app/publication";
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
    expect(html).toContain("First xcb package release in preparation");
    expect(html).toContain("No native xcb binary");
    expect(html).not.toContain(".tgz");
    expect(html).not.toContain("npm install @hraness/xcb");
  } else {
    expect(html).toContain(publishedRelease.archiveUrl);
    expect(html).toContain(publishedRelease.verificationRun);
  }
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
  expect(html).toContain("tested account hit provider quota");
  expect(html).toContain("Offline Linux ARM64");
  expect(html).toContain("No native macOS command execution");
  expect(html).toContain("Model requests still go to the provider");
  expect(html.indexOf('id="readiness"')).toBeLessThan(html.indexOf('id="install"'));
  expect(html).toContain('href="/compare"');
  expect(html).toContain('href="/docs/getting-started"');
});
