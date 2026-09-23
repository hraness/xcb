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
