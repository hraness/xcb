import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import Home from "../app/page";
import Docs from "../app/docs/page";
import Compare from "../app/compare/page";
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
  expect(html).toContain("Your subscriptions. Routed.");
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

test("the product examples are illustrative, with accessible view controls", () => {
  const html = renderToStaticMarkup(<Home />);
  const routeButtons: string[] = [];
  const workspaceButtons: string[] = [];
  new HTMLRewriter()
    .on('button[aria-controls="route-example"]', {
      element(element) { routeButtons.push(element.getAttribute("aria-pressed") ?? ""); },
    })
    .on('button[aria-controls="workspace-example"]', {
      element(element) { workspaceButtons.push(element.getAttribute("aria-pressed") ?? ""); },
    })
    .transform(html);
  expect(routeButtons).toEqual(["true", "false"]);
  expect(workspaceButtons).toEqual(["true", "false", "false"]);
  expect(html).toContain("Illustrative workspace");
  expect(html).toContain("no live provider calls");
  expect(html).toContain('aria-live="polite"');
  expect(html).not.toContain("Subagents");
});

test("the router leads and the experimental harness comes last", () => {
  const html = renderToStaticMarkup(<Home />);
  expect(html).toContain("xcb --json route");
  expect(html).toContain("createSubscriptionRouter");
  expect(html).toContain('href="/docs/route"');
  for (const [before, after] of [
    ['id="router"', 'id="interfaces"'],
    ['id="interfaces"', 'id="routing"'],
    ['id="routing"', 'id="workspace"'],
    ['id="workspace"', 'id="harness"'],
    ['id="harness"', 'id="readiness"'],
  ] as const) {
    expect(html.indexOf(before)).toBeLessThan(html.indexOf(after));
  }
  expect(html).toContain("Experimental");
  expect(html).toContain("does not execute self-modifying orchestration policies");
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
