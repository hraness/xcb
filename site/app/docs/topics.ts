import type { Metadata } from "next";
/** The docs overview's headline and meta description, shared with its share card. */
export const docsHeadline = "Set up xcb and route your first task";
export const docsDescription = "Install xcb, connect Claude, Codex, or Devin accounts, and send work through the headless CLI, JSON contract, or SDK.";

/** Sidebar and overview groups, in order: both readers, then each reader. */
export const docsGroups = [
  { name: "Start here", reader: "Install, run a first task, and see how xcb decides" },
  { name: "Daily use", reader: "Use xcb as your coding agent" },
  { name: "Build with xcb", reader: "Hand xcb tasks from agents, scripts, and apps" },
] as const;

export const docsTopics = [
  { slug: "getting-started", title: "Getting started", description: "Install xcb, connect a Claude account, and send your first task, ending with a fix xcb routed to one of your accounts and made in a practice project.", group: "Start here" },
  { slug: "how-routing-works", title: "How routing works", description: "How xcb picks an account and model for each task: which accounts can take it, how models are ranked, and why the account stays held until the provider exits.", group: "Start here" },
  { slug: "security", title: "Security and privacy", description: "What stays on your machine, what xcb sends to Claude, Codex, and Devin, what the optional judge sees, and where credentials live outside your projects.", group: "Start here" },
  { slug: "projects-and-tasks", title: "Projects and tasks", description: "Submit tasks to named project folders, inspect JSON state, add guidance, cancel with a revision, and import saved conversations without a terminal UI.", group: "Daily use" },
  { slug: "providers", title: "Accounts and models", description: "Connect Claude, Codex, or Devin accounts, check which provider builds xcb supports, and choose an account or model when you want a specific one.", group: "Daily use" },
  { slug: "workspace", title: "Tests and builds", description: "Set up the offline Linux VM that runs your project's tests and builds on macOS ARM64, prepare public dependencies, and see what the runner can't do.", group: "Daily use" },
  { slug: "customization", title: "Customize xcb", description: "Change panes, continuation, context management, the per-turn deadline, hooks, and the optional routing judge, and see where each setting is stored.", group: "Daily use" },
  { slug: "reflexes", title: "Learned routing and continuation", description: "How xcb learns which model tier you want and notices when a worker stopped short, and how to inspect, teach, and roll it back.", group: "Daily use" },
  { slug: "upgrade-and-uninstall", title: "Upgrade and uninstall", description: "Keep xcb current with verified releases, restart safely after an upgrade, and remove the binary, its background jobs, and optionally your saved state.", group: "Daily use" },
  { slug: "troubleshooting", title: "Troubleshooting", description: "Fix common problems: provider builds xcb won't run, accounts that need signing in again, runs that didn't finish cleanly, and macOS folder access.", group: "Daily use" },
  { slug: "route", title: "Route tasks", description: "Hand xcb one task as JSON with xcb --json route. It picks the account and model, runs one turn, and returns the result and how the run ended.", group: "Build with xcb" },
  { slug: "sdk", title: "TypeScript SDK", description: "Embed xcb's router in a TypeScript app: install the package, register adapters, and run tasks on the account and model your app names.", group: "Build with xcb" },
  { slug: "application-api", title: "Application API", description: "Get one model response per call for your app, with no tools or saved history, once your xcb build, account, and model pass xcb's application checks.", group: "Build with xcb" },
  { slug: "reference", title: "CLI and configuration", description: "Every xcb command grouped as in xcb --help, plus config.json settings, environment variables, files and folders, exit codes, and JSON output.", group: "Build with xcb" },
] as const satisfies readonly Readonly<{ slug: string; title: string; description: string; group: (typeof docsGroups)[number]["name"] }>[];

export type DocsTopic = (typeof docsTopics)[number];
export type DocsSlug = DocsTopic["slug"];

export function findDocsTopic(slug: string): DocsTopic | undefined {
  return docsTopics.find((topic) => topic.slug === slug);
}

export function topicMetadata(topic: DocsTopic): Metadata {
  const title = `${topic.title} · Excalibur (xcb) docs`;
  const url = `/docs/${topic.slug}`;
  return {
    title,
    description: topic.description,
    alternates: { canonical: url },
    openGraph: { title, description: topic.description, url, type: "article", siteName: "Excalibur (xcb)" },
    twitter: { card: "summary_large_image", title, description: topic.description },
  };
}
