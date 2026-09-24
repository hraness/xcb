import { resolve } from "node:path";

import { blogPosts } from "../app/blog/posts.ts";
import { publishedRelease } from "../app/publication.ts";
import { contentsFor, renderBlogBody } from "./blog-html.ts";

const siteRoot = resolve(import.meta.dir, "..");

/** The status label every post renders, from the published release record. */
export function releaseLabel(): string {
  return publishedRelease === null ? "In development" : `Latest release: v${publishedRelease.version}`;
}

if (import.meta.main) {
  const label = releaseLabel();
  const entries: Record<string, { html: string; toc: readonly { href: string; label: string }[] }> = {};
  for (const entry of blogPosts) {
    const markdown = await Bun.file(resolve(siteRoot, "content/blog", `${entry.slug}.md`)).text();
    const hasPlaceholder = markdown.includes("{{release.version}}");
    if (hasPlaceholder !== entry.statusInBody) {
      throw new Error(`${entry.slug}: statusInBody must match whether the body carries the release placeholder.`);
    }
    // The body sentence reads "Latest release: {{release.version}}", so it takes the bare version.
    const rendered = renderBlogBody(markdown, publishedRelease === null ? "none yet" : `v${publishedRelease.version}`);
    entries[entry.slug] = { html: rendered.html, toc: contentsFor(rendered.toc) };
  }
  await Bun.write(
    resolve(siteRoot, "app/blog/posts.generated.ts"),
    "// Generated from content/blog/*.md by scripts/sync-blog.ts. Do not edit.\n"
      + `export const blogStatusLabel = ${JSON.stringify(label)};\n`
      + `export const blogBodies: Readonly<Record<string, Readonly<{ html: string; toc: readonly Readonly<{ href: \`#\${string}\`; label: string }>[] }>>> = ${JSON.stringify(entries, null, 2)};\n`,
  );
}
