import type { Metadata } from "next";
import { socialImages } from "../social";

export const docsTopics = [
  { slug: "getting-started", title: "Getting started", description: "Build xcb from source, connect an account, and start your first session.", group: "Start here" },
  { slug: "providers", title: "Accounts & models", description: "Connect Claude, Codex, or Devin and choose the account and model for your work.", group: "Daily use" },
  { slug: "workspace", title: "Tests, builds & recovery", description: "Set up the isolated command runner, prepare dependencies, and recover interrupted work.", group: "Daily use" },
  { slug: "customization", title: "Make it yours", description: "Configure panes, continuation, context management, and optional extensions.", group: "Daily use" },
  { slug: "reflexes", title: "Learned routing & continuation", description: "How xcb learns which model tier you want and notices when a worker stopped short, and how to inspect, teach, and roll it back.", group: "Daily use" },
  { slug: "route", title: "Route tasks", description: "Let a coding agent or app hand xcb one task and get the result back, through the JSON route command or the TypeScript SDK.", group: "Build with xcb" },
  { slug: "application-api", title: "Application API", description: "Get one model response per call for your app, with no tools or saved history, once your exact build, account, and model pass xcb’s application checks.", group: "Build with xcb" },
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
    openGraph: { title, description: topic.description, url, type: "article", siteName: "xcb", images: socialImages },
    twitter: { card: "summary_large_image", title, description: topic.description, images: socialImages },
  };
}
