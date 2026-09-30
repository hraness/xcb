import { describe, expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { parsePublishedRelease, publicationMarkdown, publishedRelease, type NativeAsset } from "../app/publication";

const site = join(import.meta.dir, "..");
const read = async (path: string): Promise<string> => await readFile(join(site, path), "utf8");

function record(value: unknown, label: string): Readonly<Record<string, unknown>> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError(`${label} must be an object.`);
  }
  return value as Readonly<Record<string, unknown>>;
}

function stableVersion(value: unknown, label: string): readonly [bigint, bigint, bigint] {
  if (typeof value !== "string") throw new TypeError(`${label} must be a string.`);
  const match = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/u.exec(value);
  if (match === null) throw new TypeError(`${label} must be a stable version.`);
  return [BigInt(match[1]!), BigInt(match[2]!), BigInt(match[3]!)];
}

function compare(left: readonly [bigint, bigint, bigint], right: readonly [bigint, bigint, bigint]): number {
  for (let index = 0; index < 3; index += 1) {
    if (left[index] !== right[index]) return left[index]! > right[index]! ? 1 : -1;
  }
  return 0;
}

describe("xcb site source contract", () => {
  test("advertises only a verified published release that does not exceed the source version", async () => {
    const [home, releaseState, publication, packageSource] = await Promise.all([
      read("app/page.tsx"),
      read("app/release-state.tsx"),
      read("published-release.json"),
      readFile(join(site, "..", "package.json"), "utf8"),
    ]);
    const publishedRelease = record(JSON.parse(publication) as unknown, "published release");
    const packageJson = record(JSON.parse(packageSource) as unknown, "source package");
    expect(Object.keys(publishedRelease).sort()).toEqual(["archiveUrl", "native", "verificationRun", "version"]);
    const admitted = parsePublishedRelease(publishedRelease);
    // The homepage always renders the datum through the shared release-state
    // components; the datum alone decides between downloads and the honest
    // no-release state.
    expect(home).toContain('import { publishedRelease } from "./publication"');
    expect(home).toContain('from "./release-state"');
    expect(home).not.toContain("package.json");
    if (admitted === null) {
      expect(releaseState).toContain("No native release is published yet; install from source.");
      return;
    }
    const published = stableVersion(admitted.version, "published version");
    const source = stableVersion(packageJson.version, "source version");
    expect(compare(published, source)).toBeLessThanOrEqual(0);
    expect(publishedRelease.verificationRun).toMatch(/^https:\/\/github\.com\/hraness\/xcb\/actions\/runs\/[1-9][0-9]*$/u);
    expect(releaseState).toContain("release.verificationRun");
  });

  test("renders the README landing identity and the shared Ask AI links", async () => {
    const [packageJson, home, docs, generated] = await Promise.all([
      read("package.json"),
      read("app/page.tsx"),
      read("app/docs/page.tsx"),
      read("app/readme.generated.ts"),
    ]);
    expect(packageJson).toContain('"@hraness/ui": "github:hraness/ui#v0.5.18"');
    expect(packageJson).toContain('"@hraness/design-kit": "github:hraness/design-kit#v0.31.0"');
    expect(home).toContain('import { AskAiAboutThis } from "@hraness/ui"');
    expect(home).toContain('<AskAiAboutThis className="ask-ai" url="https://xcb.sh" />');
    expect(docs).toContain('<AskAiAboutThis className="ask-ai" url="https://xcb.sh/docs" />');
    expect(generated).toContain('export const readmeTitle = "Excalibur (xcb)";');
    expect(generated).toContain("export const readmeHtml = ");
  });

  test("uses the shared design-kit fonts and marketing grammar", async () => {
    const globals = await read("app/globals.css");
    expect(globals).toContain('@import "@hraness/design-kit/fonts.css"');
    expect(globals).toContain('@import "@hraness/design-kit/product-marketing.css"');
    expect(globals).toContain('@import "../vendor/paper-theme/paper-theme.css"');
    expect(globals).not.toMatch(/Georgia|Times New Roman/u);
  });

  test("sets headings in the sans text face from the shared level tokens", async () => {
    const [globals, docs, compare, calm] = await Promise.all([
      read("app/globals.css"),
      read("app/docs/docs.css"),
      read("app/compare/compare.css"),
      read("app/calm.css"),
    ]);
    expect(globals).toContain('@import "@hraness/design-kit/typography.css"');
    for (const level of ["h1", "h2", "h3"]) expect(docs).toContain(`font-size: var(--hraness-type-${level}-size)`);
    for (const level of ["h1", "h2", "h3"]) expect(calm).toContain(`font-size: var(--hraness-type-${level}-size)`);
    for (const css of [globals, docs, compare, calm]) expect(css).not.toMatch(/\bh[1-4][^{]*\{[^}]*font: 500 1\.05rem/u);
    expect(calm).toContain('[data-hraness-marketing-preset="minimal"]');
  });

  test("renders the share card from the shared template and the one site declaration", async () => {
    const [route, { socialSite }, { socialCardAlt }, mark] = await Promise.all([
      import("../app/opengraph-image"),
      import("../app/social"),
      import("../app/social-cards"),
      read("public/marks/xcb.svg"),
    ]);
    expect(route.size).toEqual({ width: 1200, height: 630 });
    expect(route.contentType).toBe("image/png");
    expect(route.alt).toBe(socialCardAlt("/"));
    expect(socialSite.name).toBe("Excalibur (xcb)");
    expect(socialSite.domain).toBe("xcb.sh");
    // The card is a crop of the header: its palette, its foil mark, its name.
    expect(socialSite.brand).toBe("Excalibur (xcb)");
    expect(socialSite.brandMark).toBe(mark);
    expect(socialSite.palette).toBe("tokyo-night");
    expect(socialSite.icon).toBeUndefined();
    expect(socialSite.mark).toBeUndefined();
    expect(socialSite.theme).toBeUndefined();
    expect(socialSite.keepTogether).toContain("Claude Code Router");
    const response = route.default();
    expect(response.headers.get("content-type")).toBe("image/png");
  });

  test("gives every page its own share card, drawn only by the shared template", async () => {
    const pages = [...new Bun.Glob("app/**/page.tsx").scanSync({ cwd: site })].map((path) => path.replace(/page\.tsx$/u, ""));
    // /download redirects to the release; it keeps the home card.
    for (const dir of pages.filter((dir) => dir !== "app/download/")) {
      const source = await read(`${dir}opengraph-image.tsx`);
      expect(source).toContain("socialImageFor(");
      expect(source).not.toMatch(/<svg|<div|new ImageResponse|createSocialImageResponse/u);
    }
    for (const path of ["app/social.ts", "app/social-cards.ts", "app/social-image.tsx"]) {
      expect(await read(path)).not.toMatch(/<svg|<div|new ImageResponse/u);
    }
    for (const dir of pages) expect(await read(`${dir}page.tsx`)).not.toContain("socialImages");
  });

  test("fits every share card's copy as written, with no finding from the template", async () => {
    const [{ socialImageFit, socialImageSiteDetails }, { socialSite }, { socialCards }, { docsTopics }, { comparisons }, { blogPosts }] = await Promise.all([
      import("@hraness/web-discovery/social-image/card"),
      import("../app/social"),
      import("../app/social-cards"),
      import("../app/docs/topics"),
      import("../app/compare/comparisons"),
      import("../app/blog/posts"),
    ]);
    expect(socialCards.size).toBe(6 + docsTopics.length + comparisons.length + blogPosts.length);
    for (const [path, page] of socialCards) {
      const fit = socialImageFit(socialImageSiteDetails(socialSite, page));
      // Findings include v0.12's review codes (reduced description, missing
      // or repeated eyebrow, trailing ellipsis, tagline reuse) as well as the
      // cut, shrink, and strip issues that strict rendering rejects.
      expect({ path, findings: fit.findings }).toEqual({ path, findings: [] });
      if (path !== "/") {
        expect(fit.layout).toBe("page");
        expect(page.description).not.toBe(socialSite.description);
      }
    }
  });

  test("keeps the sitemap and robots on the canonical origin", async () => {
    const [sitemap, robots] = await Promise.all([read("public/sitemap.xml"), read("public/robots.txt")]);
    expect(sitemap).toContain("<loc>https://xcb.sh/</loc>");
    expect(sitemap).toContain("<loc>https://xcb.sh/docs</loc>");
    expect(robots).toContain("Sitemap: https://xcb.sh/sitemap.xml");
  });

  test("keeps the llms.txt map and docs social metadata on the canonical origin", async () => {
    const [{ GET }, docs, { docsTopics }, { providerStatus, supportedBuilds }] = await Promise.all([
      import("../app/llms.txt/route"),
      read("app/docs/page.tsx"),
      import("../app/docs/topics"),
      import("../app/docs/provider-status"),
    ]);
    const llms = await GET().text();
    expect(llms).toContain("https://xcb.sh/");
    expect(llms).toContain("https://xcb.sh/docs");
    expect(llms).toContain("https://xcb.sh/README.md");
    expect(llms).not.toContain("http://");
    // The canonical one-line description leads, verbatim.
    expect(llms.split("\n")[2]).toBe("> Excalibur (xcb) routes coding tasks across the Claude, Codex, and Devin subscriptions you already pay for.");
    // Every current docs page is listed, and repository links follow main, not an old tag.
    for (const topic of docsTopics) expect(llms).toContain(`(https://xcb.sh/docs/${topic.slug})`);
    expect(llms).not.toMatch(/github\.com\/hraness\/xcb\/blob\/v\d/u);
    // The only xcb version named is the published release: no changelog narrative.
    const versions = new Set([...llms.matchAll(/\bv(\d+\.\d+\.\d+)\b/gu)].map((match) => match[1]));
    if (publishedRelease === null) {
      expect(llms).toContain("No native xcb binary");
      expect(versions.size).toBe(0);
    } else {
      expect(llms).toContain(publicationMarkdown(publishedRelease));
      expect([...versions]).toEqual([publishedRelease.version]);
    }
    for (const fact of ["xcb --json route", "dryRun", "createSubscriptionRouter", "npm install @hraness/xcb", supportedBuilds.claudeMinimum, ...supportedBuilds.codex, ...supportedBuilds.devin, "does not run self-modifying routing policies"]) {
      expect(llms).toContain(fact);
    }
    for (const status of Object.values(providerStatus)) expect(llms.split(status).length - 1).toBe(1);
    expect(docs).toContain('siteName: "Excalibur (xcb)"');
    expect(docs).toContain('card: "summary_large_image"');
  });
});


test("publication metadata fails closed without an exact xcb artifact and verification", async () => {
  expect(parsePublishedRelease({ version: null, archiveUrl: null, verificationRun: null, native: null })).toBeNull();
  const verificationRun = "https://github.com/hraness/xcb/actions/runs/123";
  const archiveUrl = "https://github.com/hraness/xcb/releases/download/v0.20.0/hraness-xcb-0.20.0.tgz";
  const native: NativeAsset[] = [
    {
      platform: "darwin-aarch64",
      url: "https://github.com/hraness/xcb/releases/download/v0.20.0/xcb-0.20.0-darwin-aarch64.tar.gz",
      sha256Url: "https://github.com/hraness/xcb/releases/download/v0.20.0/xcb-0.20.0-darwin-aarch64.tar.gz.sha256",
    },
    {
      platform: "linux-x86_64",
      url: "https://github.com/hraness/xcb/releases/download/v0.20.0/xcb-0.20.0-linux-x86_64.tar.gz",
      sha256Url: "https://github.com/hraness/xcb/releases/download/v0.20.0/xcb-0.20.0-linux-x86_64.tar.gz.sha256",
    },
  ];
  const valid = { version: "0.20.0", archiveUrl, verificationRun, native };
  expect(parsePublishedRelease(valid)).toEqual(valid);
  // Either artifact kind alone satisfies the contract.
  expect(parsePublishedRelease({ ...valid, archiveUrl: null })?.archiveUrl).toBeNull();
  expect(parsePublishedRelease({ ...valid, native: [] })?.native).toEqual([]);
  for (const value of [
    null, {},
    { version: null, archiveUrl: null, verificationRun: null },
    { ...valid, archiveUrl: null, native: [] },
    { ...valid, native: null },
    { ...valid, native: {} },
    { ...valid, native: [native[0], native[0]] },
    { ...valid, native: [{ ...native[0], platform: "darwin-x86_64" }] },
    { ...valid, native: [{ ...native[0], url: `${native[0]!.url}.other` }] },
    { ...valid, native: [{ ...native[0], sha256Url: native[0]!.url }] },
    { ...valid, native: [{ ...native[0], extra: true }] },
    { ...valid, verificationRun: null },
    { ...valid, verificationRun: "https://example.com" },
    { ...valid, version: "9007199254740992.0.0" }, { ...valid, extra: true },
    { ...valid, archiveUrl: "https://github.com/hraness/xcb/releases/download/v0.3.0/hraness-agentmixer-0.3.0.tgz" },
    { ...valid, archiveUrl: archiveUrl.replace("0.20.0.tgz", "0.19.0.tgz") },
    { version: "0.3.0", verificationRun, archiveUrl: null, native: [] },
  ]) {
    expect(() => parsePublishedRelease(value)).toThrow();
  }
  // The shipped fixture exercises the full published state without claiming one.
  const fixture = JSON.parse(await read("tests/fixtures/published-release.json")) as unknown;
  expect(parsePublishedRelease(fixture)).toEqual(valid);
});

test("publication Markdown names the verified release without offering a browser download", async () => {
  const release = parsePublishedRelease(JSON.parse(await read("tests/fixtures/published-release.json")));
  if (release === null) throw new Error("published fixture required");
  expect(publicationMarkdown(null)).toBe("");
  const markdown = publicationMarkdown(release);
  expect(markdown).toContain(`**v${release.version}**`);
  expect(markdown).toContain(release.verificationRun);
  for (const asset of release.native) {
    expect(markdown).not.toContain(asset.url);
    expect(markdown).not.toContain(asset.sha256Url);
  }
  expect(markdown).not.toContain(release.archiveUrl!);
  expect(markdown).not.toContain(".tar.gz");
});


test("loads the immutable material after Paper and editorial styling", async () => {
  const [css, layout, checker] = await Promise.all([read("app/globals.css"), read("app/layout.tsx"), read("scripts/check-paper-theme.mjs")]);
  const materialImport = '@import "../vendor/hraness-lantern/lantern-material.css";';
  expect(css).toContain(materialImport);
  expect(css.indexOf(materialImport)).toBeGreaterThan(css.indexOf('product-marketing-preset.css";'));
  expect(layout).toContain('data-hraness-material="lantern"');
  expect(checker).toContain('import { checkLanternMaterialSnapshot } from "../vendor/hraness-lantern/check.mjs"');
  expect(checker).toContain("await checkLanternMaterialSnapshot();");
});

test("registers the footer layer after UI layers in one stylesheet", async () => {
  const [css, layout] = await Promise.all([read("app/globals.css"), read("app/layout.tsx")]);
  const footer = '@import "@hraness/site-footer/styles.css";';
  expect(css).toContain(footer);
  expect(css.indexOf(footer)).toBeGreaterThan(css.indexOf('@import "@hraness/ui/styles.css";'));
  expect(css.indexOf(footer)).toBeGreaterThan(css.indexOf('lantern-material.css";'));
  expect(layout).not.toContain('import "@hraness/site-footer/styles.css"');
});

test("adopts the shared palette contract with Tokyo Night as the default appearance", async () => {
  const [layout, header, bootstrap, css, packageJson] = await Promise.all([
    read("app/layout.tsx"),
    read("app/site-header.tsx"),
    read("browser/theme-bootstrap.ts"),
    read("app/globals.css"),
    read("package.json"),
  ]);
  expect(layout).toContain('data-palette="tokyo-night"');
  expect(layout).toContain('getDesignPaletteTheme("tokyo-night", "light")');
  expect(layout).toContain('src="/theme-bootstrap.js"');
  expect(layout).toContain("DesignPaletteProvider");
  expect(layout).toContain("suppressHydrationWarning");
  // The single appearance control sits at the rightmost header action.
  expect(header).toContain('trailing={<ThemeMenuButton aria-label="Appearance" />}');
  // The blocking bootstrap installs appearance before the React menu hydrates.
  expect(bootstrap).toContain("initDesignPalette");
  // Palette themes and the semantic bridge load before the vendored theme.
  expect(css).toContain('@import "@hraness/design-kit/palettes.css";');
  expect(css.indexOf('palettes.css')).toBeLessThan(css.indexOf("vendor/paper-theme"));
  expect(packageJson).toContain('"build:theme"');
});
