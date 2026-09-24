import type { Metadata } from "next";
import { ArticleIndex } from "@hraness/design-kit/react/server";
import { blogJsonLd } from "@hraness/web-discovery";
import { JsonLdScript } from "@hraness/web-discovery/json-ld";
import { SiteHeader } from "../site-header";
import { socialImages } from "../social";
import { blogArticleDiscovery, blogPublisher, blogSite } from "./discovery";
import { blogDescription, blogFeedPath, blogPath, blogPostPath, blogTitle, indexableBlogPosts } from "./posts";

const title = "Blog · xcb";

export const metadata: Metadata = {
  title,
  description: blogDescription,
  alternates: {
    canonical: blogPath,
    types: { "application/atom+xml": [{ url: blogFeedPath, title: blogTitle }] },
  },
  openGraph: { title, description: blogDescription, siteName: "xcb", type: "website", url: blogPath, images: socialImages },
  twitter: { card: "summary_large_image", title, description: blogDescription, images: socialImages },
};

export default function Blog() {
  const jsonLd = blogJsonLd(
    blogSite,
    { name: blogTitle, description: blogDescription, path: blogPath, publisher: blogPublisher },
    indexableBlogPosts.map(blogArticleDiscovery),
  );
  return (
    <div data-hraness-marketing-preset="editorial" className="xcb-blog-page">
      <SiteHeader active="blog" />
      <main id="main" tabIndex={-1}>
        <ArticleIndex
          heading="Blog"
          headingId="blog-title"
          headingLevel={1}
          summary={blogDescription}
          items={indexableBlogPosts.map((entry) => ({
            href: blogPostPath(entry),
            title: entry.title,
            dek: entry.dek,
            published: entry.published,
            eyebrow: entry.eyebrow,
          }))}
        />
        <p className="xcb-blog-feed"><a href={blogFeedPath}>Atom feed</a></p>
      </main>
      <JsonLdScript data={jsonLd} id="blog-json-ld" />
    </div>
  );
}
