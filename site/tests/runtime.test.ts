import { describe, expect, test } from "bun:test";
import { join } from "node:path";
import { articleProvenanceFromAdmission, articleProvenanceSentence } from "@hraness/design-kit";

import { blogPostPath, blogPosts, indexableBlogPosts } from "../app/blog/posts";
import { comparisons } from "../app/compare/comparisons";
import { docsTopics } from "../app/docs/topics";
import { publishedRelease } from "../app/publication";

const site = join(import.meta.dir, "..");

async function startBuiltSite() {
  const process_ = Bun.spawn([
    join(site, "node_modules/.bin/next"),
    "start",
    "--hostname",
    "127.0.0.1",
    "--port",
    "0",
  ], {
    cwd: site,
    env: { ...process.env, NODE_ENV: "production" },
    stderr: "pipe",
    stdout: "pipe",
  });
  let output = "";
  let startupSettled = false;
  let rejectStartup: (error: Error) => void = () => {};
  let resolveStartup: (origin: string) => void = () => {};
  const startup = new Promise<string>((resolve, reject) => {
    rejectStartup = reject;
    resolveStartup = resolve;
  });
  const settleFromOutput = (): void => {
    const match = output.match(/http:\/\/127\.0\.0\.1:(\d+)/u);
    if (match === null || !output.includes("Ready in") || startupSettled) return;
    startupSettled = true;
    resolveStartup(`http://127.0.0.1:${match[1]}`);
  };
  const capture = async (stream: ReadableStream<Uint8Array>): Promise<void> => {
    const decoder = new TextDecoder();
    const reader = stream.getReader();
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        output += decoder.decode(value, { stream: true });
        settleFromOutput();
      }
      output += decoder.decode();
      settleFromOutput();
    } catch (error) {
      if (!startupSettled) {
        startupSettled = true;
        rejectStartup(error instanceof Error ? error : new Error(String(error)));
      }
    } finally {
      reader.releaseLock();
    }
  };
  const captureTasks = [capture(process_.stdout), capture(process_.stderr)];
  const exitTask = process_.exited.then((exitCode) => {
    if (startupSettled) return;
    startupSettled = true;
    rejectStartup(new Error(`Next exited with code ${exitCode} before startup.\n${output}`));
  });
  const timeout = setTimeout(() => {
    if (startupSettled) return;
    startupSettled = true;
    rejectStartup(new Error(`Next did not start within 10 seconds.\n${output}`));
  }, 10_000);
  try {
    const origin = await startup;
    clearTimeout(timeout);
    return { captureTasks, exitTask, origin, process_ };
  } catch (error) {
    clearTimeout(timeout);
    if (process_.exitCode === null) process_.kill("SIGTERM");
    await process_.exited;
    await Promise.allSettled(captureTasks);
    throw error;
  }
}

async function stopBuiltSite(server: Awaited<ReturnType<typeof startBuiltSite>>): Promise<void> {
  if (server.process_.exitCode === null) server.process_.kill("SIGTERM");
  const stoppedGracefully = await Promise.race([
    server.process_.exited.then(() => true),
    Bun.sleep(2_000).then(() => false),
  ]);
  if (!stoppedGracefully && server.process_.exitCode === null) {
    server.process_.kill("SIGKILL");
    await server.process_.exited;
  }
  await server.exitTask;
  await Promise.allSettled(server.captureTasks);
}

describe("built xcb site", () => {
  test("serves the homepage, docs, and static discovery files through Next", async () => {
    const server = await startBuiltSite();
    try {
      const [homeResponse, docsResponse, robotsResponse, llmsResponse, readmeResponse, missingResponse] = await Promise.all([
        fetch(`${server.origin}/`, { redirect: "manual" }),
        fetch(`${server.origin}/docs`, { redirect: "manual" }),
        fetch(`${server.origin}/robots.txt`, { redirect: "manual" }),
        fetch(`${server.origin}/llms.txt`, { redirect: "manual" }),
        fetch(`${server.origin}/README.md`, { redirect: "manual" }),
        fetch(`${server.origin}/missing`, { redirect: "manual" }),
      ]);
      const [home, docs, robots, llms, readme] = await Promise.all([homeResponse.text(), docsResponse.text(), robotsResponse.text(), llmsResponse.text(), readmeResponse.text()]);
      expect(homeResponse.status).toBe(200);
      expect(home).toContain(publishedRelease === null ? "No native release is published yet" : `v${publishedRelease.version}`);
      expect(home).toContain('<link rel="canonical" href="https://xcb.sh"');
      expect(home).toContain('aria-label="Ask AI about this"');
      expect(docsResponse.status).toBe(200);
      expect(docs).toContain('<link rel="canonical" href="https://xcb.sh/docs"');
      expect(docs).toContain('id="standalone-package"');
      expect(robotsResponse.status).toBe(200);
      expect(robots).toContain("Sitemap: https://xcb.sh/sitemap.xml");
      expect(llmsResponse.status).toBe(200);
      expect(llms).toContain("https://xcb.sh/docs");
      expect(readmeResponse.status).toBe(200);
      expect(readmeResponse.headers.get("content-type")).toContain("text/markdown");
      expect(readme).toContain("# Excalibur (xcb)");
      expect(readme).not.toContain("hraness:xcb-landing");
      expect(docs).toContain('og:site_name" content="Excalibur (xcb)"');
      expect(docs).toContain('twitter:title" content="Documentation · Excalibur (xcb)"');
      expect(docs).toContain('twitter:card" content="summary_large_image"');
      expect(missingResponse.status).toBe(404);

      // Follow every local link across the actual built public pages. Broken
      // doc routes or fragments must fail before publishing the marketing site.
      const paths = ["/", "/install", "/compare", ...comparisons.map((entry) => `/compare/${entry.slug}`), "/reflexes", "/docs", ...docsTopics.map((topic) => `/docs/${topic.slug}`), "/blog", ...indexableBlogPosts.map(blogPostPath)];
      // Downloads moved to the install page; the old URL keeps working.
      const download = await fetch(`${server.origin}/download`, { redirect: "manual" });
      expect(download.status).toBe(308);
      expect(new URL(download.headers.get("location") ?? "", server.origin).pathname).toBe("/install");
      const documents = new Map<string, string>();
      for (const path of paths) {
        const response = await fetch(`${server.origin}${path}`, { redirect: "manual" });
        expect(response.status).toBe(200);
        const body = await response.text();
        expect(body.match(/<h1\b/gu)).toHaveLength(1);
        expect(body).toContain('href="https://xcb.sh' + (path === "/" ? "" : path) + '"');
        // Every page carries its own share card, not the home card. Vercel
        // previews serve the file-based card from the preview origin.
        const card = `${path === "/" ? "" : path}/opengraph-image`.replaceAll("/", "\\/");
        expect(body).toMatch(new RegExp(`<meta property="og:image" content="https:\\/\\/[^"/]+${card}`, "u"));
        expect(body).toMatch(new RegExp(`<meta name="twitter:image" content="https:\\/\\/[^"/]+${card}`, "u"));
        const image = /<meta property="og:image" content="https:\/\/[^"/]+([^"]+)"/u.exec(body)?.[1] ?? "";
        const imageResponse = await fetch(`${server.origin}${image.replaceAll("&amp;", "&")}`);
        expect(imageResponse.status).toBe(200);
        expect(imageResponse.headers.get("content-type")).toBe("image/png");
        documents.set(path, body);
      }
      for (const [path, body] of documents) {
        const links: string[] = [];
        new HTMLRewriter().on("a[href]", { element(element) { links.push(element.getAttribute("href") ?? ""); } }).transform(body);
        for (const href of links) {
          const url = new URL(href.replaceAll("&amp;", "&"), `${server.origin}${path}`);
          if (url.origin !== server.origin) continue;
          if (!documents.has(url.pathname)) {
            const target = await fetch(`${server.origin}${url.pathname}`, { redirect: "manual" });
            expect(target.status).toBe(200);
            documents.set(url.pathname, await target.text());
          }
          if (url.hash) expect(documents.get(url.pathname)).toContain(`id="${decodeURIComponent(url.hash.slice(1))}"`);
        }
      }
      const sitemapResponse = await fetch(`${server.origin}/sitemap.xml`);
      expect(sitemapResponse.status).toBe(200);
      const sitemap = await sitemapResponse.text();
      for (const path of paths) expect(sitemap).toContain(`<loc>https://xcb.sh${path}</loc>`);

      // Quarantined posts are readable by link, carry noindex, and stay out of discovery files.
      const feed = await (await fetch(`${server.origin}/blog/feed.xml`)).text();
      expect(feed).toContain("<feed xmlns=\"http://www.w3.org/2005/Atom\"");
      for (const entry of blogPosts) {
        const path = blogPostPath(entry);
        const body = documents.get(path) ?? await (await fetch(`${server.origin}${path}`)).text();
        // The visible drafting and review note matches the post's own review record.
        expect(body).toContain(articleProvenanceSentence(articleProvenanceFromAdmission(entry.admission)));
        const noindex = /<meta name="robots" content="noindex/u.test(body);
        expect(noindex).toBe(entry.admission.lifecycle !== "indexable");
        if (entry.admission.lifecycle !== "indexable") {
          expect(sitemap).not.toContain(`<loc>https://xcb.sh${path}</loc>`);
          expect(feed).not.toContain(`<id>https://xcb.sh${path}</id>`);
          expect(documents.get("/blog")).not.toContain(`href="${path}"`);
        }
      }

      // The renamed introduction's old URL redirects permanently to the new post.
      const retired = await fetch(`${server.origin}/blog/introducing-xcb`, { redirect: "manual" });
      expect(retired.status).toBe(308);
      expect(new URL(retired.headers.get("location") ?? "", server.origin).pathname).toBe("/blog/introducing-excalibur");
    } finally {
      await stopBuiltSite(server);
    }
  }, 20_000);
});
