import type { Metadata } from "next";
import { RouteNotFoundPage } from "@hraness/design-kit/react";

import { blogPath, blogPostPath, blogTitle, indexableBlogPosts } from "./blog/posts";
import { docsTopics } from "./docs/topics";
import { SiteHeader } from "./site-header";

export const metadata: Metadata = {
  title: "Page not found · Excalibur (xcb)",
};

// The hint shows at most 48 characters; cut longer post titles at a word.
function routeLabel(title: string): string {
  if (title.length <= 48) return title;
  const cut = title.slice(0, 47);
  return `${cut.slice(0, cut.lastIndexOf(" ")).trimEnd()}…`;
}

// Known pages for "Did you mean": the fixed pages, every docs topic, the blog, and every listed post.
const routes = [
  { href: "/", label: "Excalibur (xcb)" },
  { href: "/docs", label: "Documentation" },
  { href: "/compare", label: "Compare" },
  { href: "/install", label: "Install" },
  { href: "/reflexes", label: "Routing that learns how you work" },
  ...docsTopics.map((topic) => ({ href: `/docs/${topic.slug}`, label: topic.title })),
  { href: blogPath, label: blogTitle },
  ...indexableBlogPosts.map((post) => ({ href: blogPostPath(post), label: routeLabel(post.title) })),
];

export default function NotFound() {
  return (
    <>
      <SiteHeader />
      <main id="main" tabIndex={-1}>
        <RouteNotFoundPage
          siteName="Excalibur (xcb)"
          primaryAction={{ href: "/docs/getting-started", label: "Install xcb" }}
          next={[
            {
              href: "/docs/providers",
              label: "Accounts & models",
              description: "Connect Claude, Codex, or Devin and choose the account and model for each task.",
            },
            {
              href: "/compare",
              label: "Compare",
              description: "Where xcb fits next to Claude Code, Codex, OpenCode, and Devin.",
            },
            {
              href: "/install",
              label: "Download",
              description: "Release archives with checksums and a public verification run, or the source build.",
            },
          ]}
          routes={routes}
          agentIndexHref="/llms.txt"
          canvasAs="div"
        />
      </main>
    </>
  );
}
