import { describe, expect, test } from "bun:test";
import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import {
  articleAdmissionPasses,
  articleProvenanceFromAdmission,
  articleProvenanceSentence,
  assertArticleAdmissions,
} from "@hraness/design-kit";
import { portfolioRelations } from "@hraness/design-kit/portfolio";
import { articleJsonLd, createBlogSitemapPaths } from "@hraness/web-discovery";
import { blogArticleDiscovery, blogRelatedProducts, blogSite } from "../app/blog/discovery";
import { blogAtomFeed } from "../app/blog/feed";
import { blogBodies, blogStatusLabel } from "../app/blog/posts.generated";
import { blogPath, blogPostPath, blogPosts, indexableBlogPosts, markdownBlogPosts } from "../app/blog/posts";
import { publishedRelease } from "../app/publication";
import nextConfig from "../next.config";
import { renderBlogBody } from "../scripts/blog-html";

const site = join(import.meta.dir, "..");
const read = async (path: string): Promise<string> => await readFile(join(site, path), "utf8");
const quarantined = blogPosts.filter((entry) => entry.admission.lifecycle !== "indexable");

describe("xcb blog", () => {
  test("every post has a valid admission record", () => {
    assertArticleAdmissions(blogPosts.map((entry) => entry.admission));
    for (const entry of blogPosts) expect(entry.admission.href).toBe(blogPostPath(entry));
  });

  test("a post is indexable only when it passes the rubric along a registered relation", () => {
    const registered = new Set(portfolioRelations.filter((relation) => relation.detail !== null).map((relation) => relation.id));
    for (const entry of indexableBlogPosts) {
      expect(articleAdmissionPasses(entry.admission.scores)).toBe(true);
      if (entry.relation !== null && entry.relation !== "all") expect(registered.has(entry.relation)).toBe(true);
    }
    for (const entry of blogPosts) {
      if (entry.relation !== null && entry.relation !== "all" && !registered.has(entry.relation)) {
        expect(entry.admission.lifecycle).toBe("quarantined");
        expect(blogRelatedProducts(entry)).toEqual([]);
      }
    }
  });

  test("resolves the Hraness party to a real entity in article JSON-LD", () => {
    const party = {
      "@type": "Organization" as const,
      name: "Hraness",
      url: "https://hraness.com",
      sameAs: ["https://github.com/hraness"],
    };
    const jsonLd = articleJsonLd(blogSite, blogArticleDiscovery(blogPosts[0]!));
    expect(jsonLd.author).toEqual([party]);
    expect(jsonLd.publisher).toEqual(party);
  });

  test("names the AI review as AI and never as human", () => {
    for (const entry of blogPosts) {
      const sentence = articleProvenanceSentence(articleProvenanceFromAdmission(entry.admission));
      expect(sentence).not.toMatch(/human/iu);
      expect(entry.admission.humanReview).toBeNull();
      if (entry.admission.review === null) {
        // A post nobody has reviewed says so on the page and stays out of discovery.
        expect(sentence).toBe("Drafted with AI from the source code. It has not been reviewed yet.");
        expect(entry.admission.lifecycle).toBe("quarantined");
      } else {
        expect(sentence).toStartWith("Drafted with AI from the source code and reviewed by ");
        expect(entry.admission.review.reviewerType).toBe("ai");
      }
    }
  });

  test("redirects retired post URLs to posts in the registry", async () => {
    const paths: string[] = blogPosts.map(blogPostPath);
    const redirects = (await nextConfig.redirects?.()) ?? [];
    expect(redirects).toContainEqual({ source: "/blog/introducing-xcb", destination: "/blog/introducing-excalibur", permanent: true });
    const sitemap = await read("public/sitemap.xml");
    for (const redirect of redirects.filter(({ source }) => source.startsWith(`${blogPath}/`))) {
      // A redirect never shadows a live post and always lands on one.
      expect(paths).not.toContain(redirect.source);
      expect(paths).toContain(redirect.destination);
      expect(redirect.permanent).toBe(true);
      expect(sitemap).not.toContain(`https://xcb.sh${redirect.source}<`);
    }
  });

  test("has one Markdown body per post and renders each one", async () => {
    const files = (await readdir(join(site, "content/blog"))).filter((name) => name.endsWith(".md")).sort();
    expect(files).toEqual(markdownBlogPosts.map(({ slug }) => `${slug}.md`).sort());
    for (const entry of markdownBlogPosts) {
      const markdown = await read(`content/blog/${entry.slug}.md`);
      const rendered = renderBlogBody(markdown, "v0.0.0");
      expect(blogBodies[entry.slug]?.html).toBeDefined();
      expect(markdown.includes("{{release.version}}")).toBe(entry.statusInBody);
      // Internal links point at a post in this registry or an existing page.
      for (const [, href] of rendered.html.matchAll(/href="(\/blog\/[^"#]+)"/gu)) {
        expect(blogPosts.map(blogPostPath)).toContain(href as `/blog/${string}`);
      }
    }
  });

  test("renders the release status from the published release record", () => {
    const expected = publishedRelease === null ? "In development" : `Latest release: v${publishedRelease.version}`;
    expect(blogStatusLabel).toBe(expected);
    for (const entry of markdownBlogPosts.filter(({ statusInBody }) => statusInBody)) {
      expect(blogBodies[entry.slug]?.html).toContain(publishedRelease === null ? "none yet" : `v${publishedRelease.version}`);
    }
  });

  test("lists only indexable posts in the sitemap, with lastmod", async () => {
    const sitemap = await read("public/sitemap.xml");
    for (const entry of createBlogSitemapPaths({ path: blogPath }, indexableBlogPosts.map(blogArticleDiscovery))) {
      expect(sitemap).toContain(`<url><loc>https://xcb.sh${entry.path}</loc><lastmod>${String(entry.lastModified)}</lastmod></url>`);
    }
    for (const entry of quarantined) expect(sitemap).not.toContain(`https://xcb.sh${blogPostPath(entry)}<`);
  });

  test("keeps quarantined posts out of the feed and llms.txt", async () => {
    const feed = blogAtomFeed();
    const { GET } = await import("../app/llms.txt/route");
    const llms = await GET().text();
    for (const entry of indexableBlogPosts) {
      expect(feed).toContain(`<id>https://xcb.sh${blogPostPath(entry)}</id>`);
      expect(llms).toContain(`https://xcb.sh${blogPostPath(entry)}`);
    }
    for (const entry of quarantined) {
      expect(feed).not.toContain(`<id>https://xcb.sh${blogPostPath(entry)}</id>`);
      expect(llms).not.toContain(blogPostPath(entry));
    }
  });
});
