import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import Compare, { metadata } from "../app/compare/page";

test("comparison has an addressable accessible table and active navigation", () => {
  const html = renderToStaticMarkup(<Compare />);
  const headings: string[] = [];
  const rows: string[] = [];
  const columns: string[] = [];
  let currentCompare = false;
  let scrollRegion = false;
  const reviewedDates: string[] = [];
  new HTMLRewriter()
    .on("h1", { element(element) { headings.push(element.getAttribute("id") ?? ""); } })
    .on('tbody th[scope="row"]', { element() { rows.push("row"); } })
    .on('thead th[scope="col"]', { element() { columns.push("column"); } })
    .on('a[href="/compare"][aria-current="page"]', { element() { currentCompare = true; } })
    .on('[role="region"][tabindex="0"][aria-labelledby="comparison-caption"]', { element() { scrollRegion = true; } })
    .on("time", { element(element) { reviewedDates.push(element.getAttribute("datetime") ?? ""); } })
    .transform(html);
  expect(headings).toEqual(["compare-title"]);
  expect(rows).toHaveLength(4);
  expect(columns).toHaveLength(3);
  expect(currentCompare).toBe(true);
  expect(scrollRegion).toBe(true);
  expect(html).toContain('<caption id="comparison-caption">');
  expect(html).toContain('href="#main"');
  expect(reviewedDates).toEqual(["2026-09-20"]);
});

test("comparison attaches official evidence and retains current support boundaries", () => {
  const html = renderToStaticMarkup(<Compare />);
  for (const source of [
    "https://code.claude.com/docs/en/overview",
    "https://github.com/openai/codex",
    "https://opencode.ai/docs/providers",
    "https://opencode.ai/docs/agents/",
    "https://docs.devin.ai/enterprise/deployment/overview",
    "https://docs.devin.ai/use-cases/gallery/batch-3-agents-best-solution",
  ]) expect(html).toContain(`href="${source}"`);
  expect(html).toContain("Native xcb is a source preview");
  expect(html).toContain("tested account hit quota before coding acceptance");
  expect(html).toContain("offline Linux with prepared public dependencies");
  expect(html).toContain("not a general fleet of agents planning and merging parallel work");
  expect(html).toContain("an unknown model name cannot activate a provider");
  expect(html).toContain("Model requests still go to the selected provider");
  expect(html).not.toContain("npm install");
  expect(html).not.toMatch(/\ba_[a-f0-9]{32}\b/u);
});

test("comparison metadata and Ask AI target its canonical public URL", () => {
  expect(metadata.alternates).toEqual({ canonical: "/compare" });
  expect(metadata.openGraph).toMatchObject({ url: "/compare", siteName: "xcb", type: "website" });
  expect(metadata.twitter).toMatchObject({ card: "summary_large_image" });
  const html = renderToStaticMarkup(<Compare />);
  expect(html).toContain(encodeURIComponent("https://xcb.sh/compare"));
  expect(html.match(/<footer\b/gu)).toBeNull();
});
