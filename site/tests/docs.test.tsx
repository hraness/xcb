import { describe, expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { renderToStaticMarkup } from "react-dom/server";
import Docs, { metadata as overviewMetadata } from "../app/docs/page";
import DocsTopicPage, { dynamicParams, generateMetadata, generateStaticParams } from "../app/docs/[slug]/page";
import { docsTopics } from "../app/docs/topics";
import { readmeHtml } from "../app/readme.generated";
import { accessibleReferenceHtml } from "../app/docs/topic-content";

async function renderTopic(slug: string): Promise<string> {
  return renderToStaticMarkup(await DocsTopicPage({ params: Promise.resolve({ slug }) }));
}

function attributeValues(html: string, selector: string, attribute: string): string[] {
  const values: string[] = [];
  new HTMLRewriter().on(selector, {
    element(element) { values.push(element.getAttribute(attribute) ?? ""); },
  }).transform(html);
  return values;
}

describe("organized documentation", () => {
  test("prerenders every linked topic and does not enable arbitrary slugs", async () => {
    expect(dynamicParams).toBe(false);
    expect(generateStaticParams()).toEqual([
      { slug: "getting-started" }, { slug: "providers" }, { slug: "workspace" },
      { slug: "customization" }, { slug: "application-api" }, { slug: "reference" },
    ]);
    const overview = renderToStaticMarkup(<Docs />);
    for (const topic of docsTopics) {
      expect(overview).toContain(`href="/docs/${topic.slug}"`);
      const html = await renderTopic(topic.slug);
      expect(html.match(/<h1\b/gu)).toHaveLength(1);
      expect(html.match(/<main\b/gu)).toHaveLength(1);
      expect(html).toContain('id="main"');
      expect(attributeValues(html, "pre", "tabindex").every((value) => value === "0")).toBe(true);
      expect(attributeValues(html, '.xcb-docs-table-wrap', "tabindex").every((value) => value === "0")).toBe(true);
      expect(attributeValues(html, '.skip-link', 'href')).toEqual(['#main']);
      expect(attributeValues(html, 'nav[aria-label="Documentation"] a[aria-current="page"]', "href"))
        .toEqual([`/docs/${topic.slug}`]);
      const aiLinks = attributeValues(html, 'nav[aria-label="Ask AI about this"] a', "href");
      expect(aiLinks.length).toBeGreaterThan(0);
      for (const href of aiLinks) {
        expect(decodeURIComponent(href)).toContain(`https://xcb.sh/docs/${topic.slug}`);
      }
    }
    await expect(renderTopic("missing-topic")).rejects.toThrow("NEXT_HTTP_ERROR_FALLBACK;404");
  });

  test("gives overview and topics their own canonical and social URLs", async () => {
    expect(overviewMetadata.alternates?.canonical).toBe("/docs");
    for (const topic of docsTopics) {
      const metadata = await generateMetadata({ params: Promise.resolve({ slug: topic.slug }) });
      expect(metadata.alternates?.canonical).toBe(`/docs/${topic.slug}`);
      expect(metadata.openGraph?.url).toBe(`/docs/${topic.slug}`);
      expect(metadata.title).toBe(`${topic.title} · XCB docs`);
      expect(metadata.description).toBe(topic.description);
    }
    await expect(generateMetadata({ params: Promise.resolve({ slug: "missing-topic" }) }))
      .rejects.toThrow("NEXT_HTTP_ERROR_FALLBACK;404");
  });

  test("preserves inbound README anchors on the new overview", () => {
    const html = renderToStaticMarkup(<Docs />);
    expect(html.match(/<h1\b/gu)).toHaveLength(1);
    expect(html).toContain('id="readiness"');
    expect(html).toContain('id="standalone-package"');
    expect(html).toContain('href="/docs/reference#readiness"');
    expect(html).toContain('href="/docs/reference#standalone-package"');
    expect(attributeValues(html, 'nav[aria-label="Documentation"] a[aria-current="page"]', "href"))
      .toEqual(["/docs"]);
  });

  test("retains the complete generated README on the reference route", async () => {
    const html = await renderTopic("reference");
    expect(html).toContain(accessibleReferenceHtml(readmeHtml));
    // Accessibility decoration must preserve all of the generated README.
    expect(accessibleReferenceHtml(readmeHtml)
      .replaceAll('<pre tabindex="0">', "<pre>")
      .replace(/<div class="xcb-docs-table-wrap" role="region" aria-label="Reference table \d+" tabindex="0"><table>/gu, "<table>")
      .replaceAll("</table></div>", "</table>"))
      .toBe(readmeHtml);
    expect(html).toContain('id="standalone-package"');
    expect(html).toContain('id="readiness"');
  });

  test("offers source installation and observed model selection without invented releases", async () => {
    const html = await renderTopic("getting-started");
    expect(html).toContain("rustup toolchain install 1.97.1 --profile minimal");
    expect(html).toContain("./scripts/install-native.sh");
    expect(html).toContain("No native XCB release");
    expect(html).toContain("npm package is published yet");
    expect(html).toContain("xcb accounts login &lt;account-id&gt;");
    expect(html).toContain("xcb accounts refresh &lt;account-id&gt;");
    expect(html).toContain("xcb chat --resume &lt;conversation-id&gt;");
    expect(html).toContain("xcb accounts add claude --plan Max");
    expect(html).not.toContain("--label");
    expect(html).toContain("xcb --cwd /absolute/path/to/your/project");
    expect(html).toContain("It is not a headless continuation command");
    expect(html).not.toMatch(/(?:npm|bun) (?:install|add) -g @hraness\/xcb/u);
  });

  test("keeps provider evidence and the application route distinct", async () => {
    const provider = await renderTopic("providers");
    expect(provider).toContain("0.155.0-alpha.2.6");
    expect(attributeValues(provider, '.xcb-docs-table-wrap[role="region"]', "aria-labelledby"))
      .toEqual(["provider-status-caption"]);
    expect(provider).toContain('<caption id="provider-status-caption">Native CLI evidence and current limits</caption>');
    expect(provider).toContain("3000.10.31");
    expect(provider).toContain("Provider quota blocked the tested coding turn");
    expect(provider).toContain("xcb accounts import-codex --source");
    expect(provider).toContain("xcb accounts import-devin --source");
    expect(provider).toContain("task execution disabled pending qualification");
    const application = await renderTopic("application-api");
    expect(application).toContain("xcb --json generate --capabilities");
    expect(application).toContain("Application qualification is separate from coding support");
    expect(application).toContain("without provider refresh or inference");
    expect(application).toContain("available: true");
    expect(application).toContain("All six fields are required");
    expect(application).toContain("close stdin");
    expect(application).toContain("90 seconds is a practical desktop integration recommendation");
    expect(application).toContain("not a protocol timing guarantee");
    expect(application).toContain("drain stdout and stderr");
    expect(application).toContain("HOME");
    expect(application).toContain("XCB_STATE");
    expect(application).toContain("never extends their lifetime");
  });

  test("documents interactive session and pane controls with generation behavior", async () => {
    const html = await renderTopic("customization");
    for (const command of ["/sessions", "/new", "/accounts", "/model", "/pane", "/pane focus", "/pane edit", "/pane generate &lt;description&gt;"]) {
      expect(html).toContain(`<code>${command}</code>`);
    }
    expect(html).toContain("Generation uses the selected account and model");
    expect(html).toContain("next idle boundary");
  });

  test("documents offline workspace setup and recovery instead of host command fallback", async () => {
    const html = await renderTopic("workspace");
    expect(html).toContain("scripts/setup-command-runner.py");
    expect(html).toContain("source checkout matching your installed native CLI");
    expect(html).toContain("--dry-run");
    expect(html).toContain("--prepare");
    expect(html).toContain("--status --cache-key CACHE_KEY_FROM_PLAN");
    expect(html).toContain("xcb recover &lt;run-id&gt; --yes");
    expect(html).toContain("Native macOS, Xcode, Simulator, and arbitrary network commands are unavailable");
    expect(html).toContain("Each file replacement is atomic; the entire batch is not a transaction");
    expect(html).toContain("Do not delete lock files");
  });

  test("uses a responsive navigation and no developer-specific account or home paths", async () => {
    const [css, content] = await Promise.all([
      readFile(join(import.meta.dir, "../app/docs/docs.css"), "utf8"),
      readFile(join(import.meta.dir, "../app/docs/topic-content.tsx"), "utf8"),
    ]);
    expect(css).toContain("@media (max-width: 48rem)");
    expect(css).toContain(".xcb-docs-sidebar { position: static;");
    expect(css).toContain('a[aria-current="page"]');
    expect(css).toContain(".xcb-docs-body pre:focus-visible");
    expect(css).toContain(".xcb-docs-table-wrap:focus-visible");
    expect(content).not.toMatch(/\/Users\/[a-z]|a_[a-f0-9]{32}|98c6cc850848/u);
  });
});
