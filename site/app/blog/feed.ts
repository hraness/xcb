import { createAtomFeed, createFeedEntry } from "@hraness/web-discovery";
import { blogArticleDiscovery, blogFeed, blogSite } from "./discovery";
import { blogBodies } from "./posts.generated";
import { indexableBlogPosts } from "./posts";

/** The Atom feed. Only indexable posts enter it; quarantined posts stay reachable by link alone. */
export function blogAtomFeed(): string {
  return createAtomFeed(blogSite, blogFeed, indexableBlogPosts.map((entry) => (
    createFeedEntry(blogArticleDiscovery(entry), { contentHtml: blogBodies[entry.slug]?.html })
  )));
}
