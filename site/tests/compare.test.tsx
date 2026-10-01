import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { renderToStaticMarkup } from "react-dom/server";
import Compare from "../app/compare/page";
import { ComparisonPage } from "../app/compare/comparison-page";
import { comparisonPath, comparisons, devinStatus } from "../app/compare/comparisons";
import { hubDescription, hubGroups } from "../app/compare/hub";

const sitemap = readFileSync(resolve(import.meta.dir, "../public/sitemap.xml"), "utf8");
const internalTerms = /\b(?:admission|admitted|qualification|qualified|custody|settled|settlement|receipt|bounded)\b/iu;

describe("the comparison pages", () => {
  test("cover the tools people weigh against xcb, each with its own page in the sitemap", () => {
    const slugs = comparisons.map(({ slug }) => slug);
    for (const slug of ["herdr", "pi", "claude-code", "conductor", "claude-code-router", "opencode", "openrouter"]) {
      expect(slugs).toContain(slug);
      expect(sitemap).toContain(`<loc>https://xcb.sh/compare/${slug}</loc>`);
    }
    expect(new Set(slugs).size).toBe(slugs.length);
  });

  for (const entry of comparisons) {
    test(`${entry.slug}: one h1, a dated verdict, both picks, a table, and dated sources`, () => {
      const html = renderToStaticMarkup(<ComparisonPage entry={entry} />);
      expect(html.match(/<h1\b/gu)).toHaveLength(1);
      expect(html).toContain(`dateTime="${entry.updated}"`);
      expect(html).toContain(`Pick ${entry.tool} when`);
      expect(html).toContain("Pick xcb when");
      expect(entry.rows.length).toBeGreaterThanOrEqual(5);
      expect(entry.rows.length).toBeLessThanOrEqual(8);
      expect(html.match(/<tr>/gu)?.length).toBe(entry.glance.length + 1);
      expect(html).toContain("Read the full comparison");
      expect(html).toContain("data-comparison-status=");
      expect(entry.sources.length).toBeGreaterThan(0);
      for (const source of entry.sources) {
        expect(html).toContain(`href="${source.href}"`);
        expect(html).toContain(`dateTime="${source.checkedOn}"`);
      }
      expect(entry.description.length).toBeGreaterThanOrEqual(110);
      expect(entry.description.length).toBeLessThanOrEqual(160);
      expect(entry.title.length).toBeLessThanOrEqual(70);
      // Public copy: no internal vocabulary, no em dashes, no outdated status.
      const text = html.replace(/<[^>]+>/gu, " ");
      expect(text).not.toMatch(internalTerms);
      expect(text).not.toContain("—");
      expect(text).not.toContain("source preview");
      expect(text).not.toContain("npm install -g @hraness/xcb");
      expect(html).toContain('href="/install"');
    });
  }

  test("the hub links every comparison page and states Devin's status once", () => {
    const html = renderToStaticMarkup(<Compare />);
    expect(html.match(/<h1\b/gu)).toHaveLength(1);
    for (const entry of comparisons) expect(html).toContain(`href="${comparisonPath(entry.slug)}"`);
    for (const group of hubGroups) expect(html).toContain(`id="${group.id}"`);
    expect(html.split(devinStatus.replaceAll("’", "’")).length - 1).toBe(1);
    expect(hubDescription.length).toBeLessThanOrEqual(160);
    expect(html).not.toContain("—");
  });
});
