import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";

import Home from "../app/page";
import Docs from "../app/docs/page";
import { publishedRelease } from "../app/publication";
import RootLayout from "../app/layout";
import { siteDefaultPalette } from "../palette";

test("the appearance menu starts with the bootstrap's Paper/system preference", () => {
  const html = renderToStaticMarkup(<RootLayout><Home /></RootLayout>);
  const selected: string[] = [];
  new HTMLRewriter()
    .on('.hraness-design-palette-menu input[type="radio"][checked]', {
      element(element) { selected.push(element.getAttribute("value") ?? ""); },
    })
    .transform(html);
  expect(siteDefaultPalette).toEqual({ palette: "paper", mode: "system" });
  expect(selected).toEqual([siteDefaultPalette.palette, siteDefaultPalette.mode]);
});

test("every public route has one optional support footer without product signup", () => {
  for (const Page of [Home, Docs]) {
    const html = renderToStaticMarkup(<RootLayout><Page /></RootLayout>);
    expect(html.match(/<footer\b/gu)).toHaveLength(1);
    expect(html).toContain("https://account.hraness.com/support?product=xcb&amp;source=web#support");
    expect(html).toContain("Support ongoing development of a local, composable terminal workspace for coding agents.");
    expect(html).not.toContain('type="email"');
    expect(html).not.toContain('source=web#updates');
  }
});

test("the homepage binds release downloads to an exact verified xcb archive", () => {
  const html = renderToStaticMarkup(<Home />);
  expect(html.match(/<h1\b/gu)).toHaveLength(1);
  expect(html).toContain("Your agents. Your terminal. Your edge.");
  if (publishedRelease === null) {
    expect(html).toContain("First xcb package release in preparation");
    expect(html).not.toContain(".tgz");
  } else {
    expect(html).toContain(publishedRelease.archiveUrl);
    expect(html).toContain("@hraness/xcb");
    expect(html).toContain(publishedRelease.verificationRun);
  }
  expect(html).not.toContain("hraness.com/agentmixer");
});

test("the docs page renders the README with its package anchor", () => {
  const html = renderToStaticMarkup(<Docs />);
  expect(html).toContain('id="standalone-package"');
  expect(html).toContain('id="readiness"');
  expect(html).toContain("docs/compatibility.md");
  expect(html).not.toContain("data-hraness-marketing-preset");
});

test("scopes the editorial preset to the homepage header and real contract example", () => {
  const html = renderToStaticMarkup(<Home />);
  const elements: string[] = [];
  new HTMLRewriter()
    .on('[data-hraness-marketing-preset="editorial"] .hraness-marketing-header.hraness-material-chrome', {
      element() { elements.push("header"); },
    })
    .on('[data-hraness-marketing-preset="editorial"] #main .hraness-material-wall .hraness-marketing-proof-frame.hraness-material-pane', {
      element() { elements.push("proof"); },
    })
    .transform(html);
  expect(elements).toEqual(["header", "proof"]);
  expect(html).toContain("The pane is a declaration, not a fork of the harness.");
  expect(html).toContain("Less activity. More signal.");
});


test("makes provider and tool limitations visible before installation", () => {
  const html = renderToStaticMarkup(<Home />);
  expect(html).toContain("not yet a daily-driver replacement");
  expect(html).toContain("0.155.0-alpha.2.6");
  expect(html).toContain("3000.10.31");
  expect(html).toContain("Authenticated read/write/read acceptance passed");
  expect(html).toContain("tested account reached provider quota");
  expect(html).toContain("Codex and Devin task execution remains disabled pending qualification");
  expect(html).toContain("isolated Linux runner for offline tests and builds");
  expect(html).toContain("passed its 12-case VM boundary suite");
  expect(html).toContain("offline Cargo/Bun use from immutable caches");
  expect(html).toContain("Installed coding-workflow acceptance is pending");
  expect(html.indexOf('id="readiness"')).toBeLessThan(html.indexOf('id="install"'));
  expect(html).toContain("./scripts/install-native.sh");
});
