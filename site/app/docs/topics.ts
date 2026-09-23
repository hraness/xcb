import type { Metadata } from "next";

export const docsTopics = [
  { slug: "getting-started", title: "Getting started", description: "Build xcb from source, connect an account, and start your first session.", group: "Start here" },
  { slug: "providers", title: "Accounts & models", description: "Connect Claude, Codex, or Devin and choose the account and model for your work.", group: "Daily use" },
  { slug: "workspace", title: "Tests, builds & recovery", description: "Set up the isolated command runner, prepare dependencies, and recover interrupted work.", group: "Daily use" },
  { slug: "customization", title: "Make it yours", description: "Configure panes, continuation, context management, and optional extensions.", group: "Daily use" },
  { slug: "reflexes", title: "Learned routing & continuation", description: "How xcb learns which model tier you want and notices when a worker stopped short, and how to inspect, teach, and roll it back.", group: "Daily use" },
  { slug: "route", title: "Route tasks", description: "Give a coding agent or application one bounded routed turn through the closed JSON contract or the TypeScript SDK.", group: "Build with xcb" },
  { slug: "application-api", title: "Application API", description: "Use qualified, ephemeral inference while keeping application actions in your own host.", group: "Build with xcb" },
  { slug: "reference", title: "Complete reference", description: "The current repository README, including CLI examples and compatibility links.", group: "Build with xcb" },
] as const;

export type DocsTopic = (typeof docsTopics)[number];
export type DocsSlug = DocsTopic["slug"];

export function findDocsTopic(slug: string): DocsTopic | undefined {
  return docsTopics.find((topic) => topic.slug === slug);
}

export function topicMetadata(topic: DocsTopic): Metadata {
  const title = `${topic.title} · xcb docs`;
  const url = `/docs/${topic.slug}`;
  return {
    title,
    description: topic.description,
    alternates: { canonical: url },
    openGraph: { title, description: topic.description, url, type: "article", siteName: "xcb" },
    twitter: { card: "summary_large_image", title, description: topic.description },
  };
}
