import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import Reflexes, { metadata } from "../app/reflexes/page";

test("reflexes use case has one heading, canonical metadata, and reference links", () => {
  const html = renderToStaticMarkup(<Reflexes />);
  const headings: string[] = [];
  new HTMLRewriter().on("h1", { element(element) { headings.push(element.getAttribute("id") ?? ""); } }).transform(html);
  expect(headings).toEqual(["reflexes-title"]);
  expect(metadata.alternates?.canonical).toBe("/reflexes");
  expect(html).toContain('href="/docs/reflexes"');
  expect(html).toContain("held-out");
  expect(html).toContain('href="#main"');
});

test("reflexes metadata and hero describe the current release without version history", () => {
  const html = renderToStaticMarkup(<Reflexes />);
  expect(metadata.title).toBe("Stop typing “continue” to your coding agent · Excalibur (xcb)");
  expect(String(metadata.description)).toContain("only after your own replies certify it");
  expect(html).toContain('href="/install"');
  expect(html).toContain("docs/reflexes.md#measured-on-operator-history");
  for (const stale of ["v0.5.0", "v0.6.0", "source preview", "receipt"]) expect(html).not.toContain(stale);
});
