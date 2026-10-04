import type { Metadata } from "next";
import { notFound } from "next/navigation";
import { articleProvenanceFromAdmission, isArticleIndexable } from "@hraness/design-kit";
import { ArticleRelatedProducts, ArticleSources, MarketingArticle } from "@hraness/design-kit/react/server";
import { articleJsonLd, createArticleMetadata, NOINDEX_ROBOTS } from "@hraness/web-discovery";
import { JsonLdScript } from "@hraness/web-discovery/json-ld";
import { SiteHeader } from "../../site-header";
import { blogArticleDiscovery, blogRelatedProducts, blogSite } from "../discovery";
import { blogBodies } from "../posts.generated";
import { blogAuthor, blogFeedPath, blogTitle, findBlogPost, markdownBlogPosts } from "../posts";

export const dynamicParams = false;

export function generateStaticParams() {
  // A beats post has its own route beside this one.
  return markdownBlogPosts.map(({ slug }) => ({ slug }));
}

export async function generateMetadata({ params }: { params: Promise<{ slug: string }> }): Promise<Metadata> {
  const entry = findBlogPost((await params).slug);
  if (!entry) notFound();
  const metadata = createArticleMetadata(blogSite, blogArticleDiscovery(entry));
  return {
    ...metadata,
    title: `${entry.title} · Excalibur (xcb)`,
    alternates: {
      ...metadata.alternates,
      types: { "application/atom+xml": [{ url: blogFeedPath, title: blogTitle }] },
    },
    // A quarantined post is readable by link but stays out of search indexes.
    ...(isArticleIndexable(entry.admission) ? {} : { robots: NOINDEX_ROBOTS }),
  };
}

export default async function BlogPost({ params }: { params: Promise<{ slug: string }> }) {
  const entry = findBlogPost((await params).slug);
  const body = entry === undefined ? undefined : blogBodies[entry.slug];
  if (!entry || !body) notFound();
  const related = blogRelatedProducts(entry);
  return (
    <div data-hraness-marketing-preset="minimal" className="xcb-blog-page">
      <SiteHeader active="blog" />
      <main id="main" tabIndex={-1}>
        <MarketingArticle
          showDates={false}
          author={blogAuthor}
          dek={entry.dek}
          eyebrow={entry.eyebrow}
          heading={entry.title}
          provenance={articleProvenanceFromAdmission(entry.admission)}
          published={entry.published}
          updated={entry.updated}
          toc={body.toc}
          after={(
            <>
              <ArticleSources showDates={false} sources={entry.sources} />
              {related.length === 0 ? null : <ArticleRelatedProducts items={related} />}
              <p className="xcb-blog-back"><a className="hraness-text-link" href="/blog">← All posts</a></p>
            </>
          )}
        >
          <div dangerouslySetInnerHTML={{ __html: body.html }} />
        </MarketingArticle>
      </main>
      <JsonLdScript data={articleJsonLd(blogSite, blogArticleDiscovery(entry))} id="article-json-ld" />
    </div>
  );
}
