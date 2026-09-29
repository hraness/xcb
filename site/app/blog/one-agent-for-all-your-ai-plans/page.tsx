import type { Metadata } from "next";
import { articleProvenanceFromAdmission, isArticleIndexable } from "@hraness/design-kit";
import { SocialKitPanel } from "@hraness/design-kit/react";
import { ArticleRelatedProducts, ArticleSources, ArticleVideo, LaunchBeats, MarketingArticle } from "@hraness/design-kit/react/server";
import { articleJsonLd, createArticleMetadata, NOINDEX_ROBOTS } from "@hraness/web-discovery";
import { JsonLdScript } from "@hraness/web-discovery/json-ld";

import { launchBeats, launchPostSlug, socialKit } from "../../launch/beats";
import { launchFilm } from "../../launch/film";
import { BeatVisual } from "../../mockups/beat-visual";
import { RouterShowcase } from "../../mockups/showcase";
import { SiteHeader } from "../../site-header";
import "../../mockups/mockups.css";
import { blogArticleDiscovery, blogRelatedProducts, blogSite } from "../discovery";
import { blogAuthor, blogFeedPath, blogTitle, findBlogPost } from "../posts";

function entry() {
  const found = findBlogPost(launchPostSlug);
  if (found === undefined || found.format !== "beats") throw new Error(`${launchPostSlug} must be a beats post in the blog registry.`);
  return found;
}

export function generateMetadata(): Metadata {
  const post = entry();
  const metadata = createArticleMetadata(blogSite, blogArticleDiscovery(post));
  return {
    ...metadata,
    title: `${post.title} · Excalibur (xcb)`,
    alternates: {
      ...metadata.alternates,
      types: { "application/atom+xml": [{ url: blogFeedPath, title: blogTitle }] },
    },
    // Until an independent review passes, the post is readable by link and stays out of search indexes.
    ...(isArticleIndexable(post.admission) ? {} : { robots: NOINDEX_ROBOTS }),
  };
}

/** Where a reader goes next, named for the task. */
const GO_DEEPER = [
  { href: "/blog/introducing-excalibur", label: "Follow one task from sign-in to its recorded outcome" },
  { href: "/docs/how-routing-works", label: "See how xcb picks an account" },
  { href: "/docs/remote-operations", label: "Link your machines" },
  { href: "/compare", label: "Compare xcb with other agent tools" },
] as const;

export default function IntroducingExcalibur() {
  const post = entry();
  const related = blogRelatedProducts(post);
  return (
    <div data-hraness-marketing-preset="minimal" className="xcb-blog-page xcb-launch-page">
      <SiteHeader active="blog" />
      <main id="main" tabIndex={-1}>
        <MarketingArticle
          author={blogAuthor}
          dek={post.dek}
          eyebrow={post.eyebrow}
          heading={post.title}
          provenance={articleProvenanceFromAdmission(post.admission)}
          published={post.published}
          after={(
            <>
              <ArticleSources sources={post.sources} />
              {related.length === 0 ? null : <ArticleRelatedProducts items={related} />}
              <p className="xcb-blog-back"><a href="/blog">← All posts</a></p>
            </>
          )}
        >
          {launchFilm === null ? (
            <div className="xcb-launch-showcase"><RouterShowcase /></div>
          ) : (
            <ArticleVideo caption={launchFilm.caption} video={launchFilm.video} width="wide" />
          )}

          <LaunchBeats beats={launchBeats} renderVisual={(beat) => <BeatVisual beat={beat} />} />

          <h2 id="go-deeper">Go deeper</h2>
          <ul>
            {GO_DEEPER.map((link) => <li key={link.href}><a href={link.href}>{link.label}</a></li>)}
          </ul>

          {/* A quarantined post has no social kit on the page; kb/launch/social-kit.md carries it for review. */}
          {isArticleIndexable(post.admission) ? <SocialKitPanel kit={socialKit} /> : null}
        </MarketingArticle>
      </main>
      <JsonLdScript data={articleJsonLd(blogSite, blogArticleDiscovery(post))} id="article-json-ld" />
    </div>
  );
}
