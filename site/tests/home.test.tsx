import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import Home from "../app/page";
import Docs from "../app/docs/page";
import Compare from "../app/compare/page";
import { publishedRelease } from "../app/publication";
import { providerStatus } from "../app/docs/provider-status";
import Install from "../app/install/page";
import { agentPrompt, installCommand, windowsInstallCommand } from "../app/install/commands";
import RootLayout from "../app/layout";
import { siteDefaultPalette } from "../palette";


/** Visible text: tags removed and the entities React and the highlighter emit decoded. */
function textOf(html: string): string {
  return html
    .replace(/<[^>]+>/gu, "")
    .replaceAll("&quot;", '"').replaceAll("&#x27;", "'").replaceAll("&#39;", "'")
    .replaceAll("&lt;", "<").replaceAll("&gt;", ">").replaceAll("&amp;", "&");
}

test("the appearance menu agrees with the bootstrap Tokyo Night/system preference", () => {
  const html = renderToStaticMarkup(<RootLayout><Home /></RootLayout>);
  const selected: string[] = [];
  new HTMLRewriter().on('.hraness-design-palette-menu input[type="radio"][checked]', {
    element(element) { selected.push(element.getAttribute("value") ?? ""); },
  }).transform(html);
  expect(siteDefaultPalette).toEqual({ palette: "tokyo-night", mode: "system" });
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

test("the homepage leads with the registry headline and installs with one command", () => {
  const html = renderToStaticMarkup(<Home />);
  expect(html.match(/<h1\b/gu)).toHaveLength(1);
  const heroHeadings: string[] = [];
  new HTMLRewriter().on("h1#hero-title", { text(chunk) { heroHeadings.push(chunk.text); } }).transform(html);
  expect(heroHeadings.join("").trim()).toBe("Use your Claude, Codex, and Devin plans from one agent.");
  const heroSummary: string[] = [];
  new HTMLRewriter().on('header[aria-labelledby="hero-title"] .hraness-marketing-hero__summary', { text(chunk) { heroSummary.push(chunk.text); } }).transform(html);
  for (const feature of ["subscriptions", "quota", "Claude", "Codex"]) expect(heroSummary.join("")).toContain(feature);
  if (publishedRelease === null) {
    expect(html).toContain("./scripts/install-native.sh");
  } else {
    expect(textOf(html)).toContain("curl -fsSL https://xcb.sh/install.sh | sh");
    expect(html).toContain(`Latest release: v${publishedRelease.version}`);
  }
  // Installation is a command, never a browser download.
  expect(html).not.toContain(".tar.gz");
  expect(html).not.toContain(".tgz");
  expect(html).not.toContain("source preview");
  expect(html).toContain('href="/install"');
});

test("the homepage gives both readers a way in and states each limit once", () => {
  const html = renderToStaticMarkup(<Home />);
  expect(textOf(html)).toContain("xcb --json route");
  expect(html).toContain('href="/docs/route"');
  expect(html).toContain('href="/docs/getting-started"');
  expect(html).toContain("the app names the account and model");
  for (const status of Object.values(providerStatus)) expect(html).toContain(status.replaceAll("'", "&#x27;"));
  expect(html).toContain("offline in a Linux VM");
  expect(html).toContain("Git is read-only");
  expect(html).toContain("Native macOS builds cannot run");
  expect(html).toContain("herdr");
  expect(html).toContain("does not lift provider usage limits");
  for (const [before, after] of [
    ['id="router"', 'id="use"'],
    ['id="use"', 'id="import-sessions"'],
    ['id="import-sessions"', 'id="install"'],
    ['id="install"', 'id="readiness"'],
    ['id="readiness"', 'id="questions"'],
  ] as const) {
    expect(html.indexOf(before)).toBeLessThan(html.indexOf(after));
  }
});

test("the install page offers one command, copyable highlighted code, and a prompt for your agent", () => {
  const html = renderToStaticMarkup(<Install />);
  expect(html.match(/<h1\b/gu)).toHaveLength(1);
  const text = textOf(html);
  expect(text).toContain(installCommand);
  expect(html).toContain("syntax-code");
  expect(html).toContain('aria-label="Copy setup prompt"');
  expect(html.match(/xcb-code-copy/gu)?.length ?? 0).toBeGreaterThanOrEqual(6);
  expect(text).toContain(agentPrompt);
  expect(text).toContain("npm install @hraness/xcb");
  expect(html).toContain('id="source"');
  expect(html).toContain("glibc 2.34");
  expect(html).not.toContain(".tar.gz");
  expect(agentPrompt).toContain(installCommand);
  expect(agentPrompt).toContain(windowsInstallCommand);
  expect(agentPrompt).toContain("WSL2");
  const targets: string[] = [];
  new HTMLRewriter().on('#agent a[data-agent-target]', {
    element(element) { targets.push(element.getAttribute('data-agent-target') ?? ''); },
  }).transform(html);
  expect(targets).toEqual(['dot', 'grok-bot', 'muse', 'cursor', 'codex-app', 'devin']);
});

test("the shared header keeps a named home link and exact-artwork foil fallback", () => {
  for (const Page of [Home, Docs, Compare]) {
    const html = renderToStaticMarkup(<Page />);
    const homeLinks: string[] = [];
    const marks: string[] = [];
    const fallbackImages: string[] = [];
    const masks: string[] = [];
    new HTMLRewriter()
      .on('header a[aria-label="Excalibur (xcb) home"]', {
        element(element) {
          homeLinks.push(element.getAttribute("href") ?? "");
          expect(element.hasAttribute("data-foil")).toBe(true);
        },
      })
      .on('header a[aria-label="Excalibur (xcb) home"] .hraness-foil-mark', {
        element(element) { marks.push(element.getAttribute("aria-hidden") ?? ""); },
      })
      .on('header a[aria-label="Excalibur (xcb) home"] .hraness-foil-mark img', {
        element(element) {
          fallbackImages.push(element.getAttribute("src") ?? "");
          expect(element.hasAttribute("alt")).toBe(true);
          expect(element.getAttribute("alt") ?? "").toBe("");
        },
      })
      .on('header a[aria-label="Excalibur (xcb) home"] .hraness-foil-mark__paint', {
        element(element) { masks.push((element.getAttribute("style") ?? "").replaceAll("&quot;", '"')); },
      })
      .transform(html);
    expect(homeLinks).toEqual(["/"]);
    expect(marks).toEqual(["true"]);
    expect(fallbackImages).toEqual(["/marks/xcb.svg"]);
    expect(masks).toEqual(['--hraness-foil-mask:url("/marks/xcb.svg")']);
  }
});

test("keeps help output out of the hero and shows highlighted commands in terminal frames", () => {
  const html = renderToStaticMarkup(<Home />);
  const heroCode: string[] = [];
  const terminalCode: string[] = [];
  new HTMLRewriter()
    .on('header[aria-labelledby="hero-title"] pre', { element(element) { heroCode.push(element.tagName); } })
    .on('.xcb-code--terminal code.syntax-code', { element(element) { terminalCode.push(element.getAttribute("data-language") ?? ""); } })
    .transform(html);
  expect(heroCode).toEqual([]);
  expect(terminalCode).toContain("shell");
  expect(html).toContain("hraness-marketing-proof-frame__chrome");
  expect(html).not.toContain("--help");
  expect(html).not.toContain("s_…");
});

test("the homepage declares one website and one application for xcb, published by Hraness", () => {
  const html = renderToStaticMarkup(<Home />);
  const scripts = [...html.matchAll(/<script type="application\/ld\+json">([\s\S]*?)<\/script>/gu)].map((match) => match[1] ?? "");
  expect(scripts.length).toBeGreaterThan(0);
  const nodes = scripts.flatMap((script) => {
    const parsed: unknown = JSON.parse(script);
    return Array.isArray(parsed) ? parsed : [parsed];
  }) as Record<string, unknown>[];
  const ofType = (type: string) => nodes.filter((node) => node["@type"] === type);
  const [website] = ofType("WebSite");
  const [app] = ofType("SoftwareApplication");
  expect(ofType("WebSite")).toHaveLength(1);
  expect(ofType("SoftwareApplication")).toHaveLength(1);
  expect(ofType("SoftwareSourceCode")).toHaveLength(1);
  expect(ofType("FAQPage")).toHaveLength(1);
  expect(website?.["alternateName"]).toEqual(["xcb", "Excalibur"]);
  expect(website?.["publisher"]).toEqual({ "@type": "Organization", "@id": "https://hraness.com/#organization", name: "Hraness", url: "https://hraness.com/" });
  expect(app?.["@id"]).toBe("https://xcb.sh/#app");
  expect(app?.["publisher"]).toEqual({ "@type": "Organization", "@id": "https://hraness.com/#organization", name: "Hraness", url: "https://hraness.com/" });
  expect(app?.["softwareVersion"]).toBe(publishedRelease?.version);
  expect(app).not.toHaveProperty("aggregateRating");
  expect(ofType("SoftwareSourceCode")[0]?.["targetProduct"]).toEqual({ "@id": "https://xcb.sh/#app" });
});

test("home and install pages offer one install tab per platform with the released commands", () => {
  for (const Page of [Home, Install]) {
    const html = renderToStaticMarkup(<Page />);
    const text = textOf(html);
    expect(text).toContain(installCommand);
    expect(text).toContain(windowsInstallCommand);
    const mac = text.indexOf("macOS");
    const linux = text.indexOf("Linux", mac);
    const windows = text.indexOf("Windows", linux);
    expect(mac).toBeGreaterThanOrEqual(0);
    expect(linux).toBeGreaterThan(mac);
    expect(windows).toBeGreaterThan(linux);
    expect(text).toContain("x86_64 and ARM64, glibc 2.34+");
    expect(text).toContain("providers run in WSL2");
    expect(text).toContain("Runs on");
  }
  const home = renderToStaticMarkup(<Home />);
  expect(home).toContain('"operatingSystem":"macOS, Linux, Windows"');
  expect(home).not.toContain("Linux x86_64. On Linux");
});
