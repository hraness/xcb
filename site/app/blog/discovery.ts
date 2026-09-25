import { relatedFor } from "@hraness/design-kit/portfolio";
import type { ArticleDiscovery, FeedDiscovery, SearchSite } from "@hraness/web-discovery";
import { socialImageAlt } from "../social";
import { blogAuthor, blogDescription, blogFeedPath, blogPath, blogPostPath, blogTimestamp, blogTitle, type BlogPost } from "./posts";

export const blogSite: SearchSite = {
  name: "xcb",
  origin: "https://xcb.sh",
  title: blogTitle,
  description: blogDescription,
  language: "en-US",
  locale: "en_US",
  publisher: "Hraness",
};

export const blogPublisher = { kind: "Organization", name: blogAuthor.name } as const;

export function blogArticleDiscovery(entry: BlogPost): ArticleDiscovery {
  return {
    type: "BlogPosting",
    canonicalPath: blogPostPath(entry),
    blogPath,
    title: entry.title,
    description: entry.dek,
    publishedTime: blogTimestamp(entry.published),
    authors: [blogPublisher],
    publisher: blogPublisher,
    section: entry.eyebrow,
    keywords: entry.keywords,
    citations: entry.sources.map(({ href }) => href as `https://${string}`),
    image: {
      path: "/opengraph-image",
      contentType: "image/png",
      width: 1200,
      height: 630,
      alt: socialImageAlt,
    },
  };
}

export const blogFeed: FeedDiscovery = {
  title: blogTitle,
  description: blogDescription,
  homePath: blogPath,
  path: blogFeedPath,
  authors: [blogPublisher],
};

/**
 * Sibling products for a post, only along registered portfolio relations: the
 * relation a "How xcb uses X" post expands, or every registered xcb relation
 * for the introduction. An unregistered relation renders nothing.
 */
export function blogRelatedProducts(entry: BlogPost) {
  if (entry.relation === null) return [];
  const items = relatedFor("xcb");
  const chosen = entry.relation === "all" ? items : items.filter((item) => item.relationId === entry.relation);
  return chosen.slice(0, 3).map(({ name, href, role, relationship }) => ({ name, href, role, relationship }));
}
