import { addHeadingIds, assertFragmentsResolve, assertSafeTarget, headingText } from "./readme-html.ts";

export const RELEASE_PLACEHOLDER = "{{release.version}}";

export type RenderedBlogBody = Readonly<{
  html: string;
  toc: readonly Readonly<{ href: `#${string}`; label: string }>[];
}>;

/**
 * Render one post body. The release placeholder becomes the published release
 * label so no version is typed into a post; raw HTML in the Markdown is not
 * allowed; every link must be a web, mail, or site-relative target.
 */
export function renderBlogBody(markdown: string, releaseLabel: string): RenderedBlogBody {
  const source = markdown.replaceAll(RELEASE_PLACEHOLDER, releaseLabel);
  if (source.includes("{{")) throw new Error("Blog post contains an unknown template placeholder.");
  const html = Bun.markdown.html(source, { noHtmlBlocks: true, noHtmlSpans: true, tagFilter: true });
  for (const match of html.matchAll(/\s(?:href|src)="([^"]*)"/gu)) {
    const target = match[1];
    if (target === undefined) continue;
    assertSafeTarget(target);
    if (target.startsWith("//") || /^https?:\/\/(?:www\.)?xcb\.sh(?:\/|$)/u.test(target)) {
      throw new Error(`Blog post links to xcb.sh with an absolute URL; use a site-relative path: ${target}`);
    }
  }
  if (/<h1[\s>]/u.test(html)) throw new Error("Blog post bodies start at h2; the article title is the page's h1.");
  const rendered = addHeadingIds(html);
  assertFragmentsResolve(rendered);
  const toc = Array.from(rendered.matchAll(/<h2 id="([^"]+)">([\s\S]*?)<\/h2>/gu), ([, id, body]) => ({
    href: `#${id!}` as const,
    label: headingText(body!),
  }));
  return { html: rendered, toc };
}

/** The shared article component shows a contents list only with four or more sections. */
export function contentsFor(toc: RenderedBlogBody["toc"]): RenderedBlogBody["toc"] {
  return toc.length >= 4 ? toc.slice(0, 8) : [];
}
