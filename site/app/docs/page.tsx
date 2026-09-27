import type { Metadata } from "next";
import { AskAiAboutThis } from "@hraness/ui";
import { publishedRelease } from "../publication";
import { ReleaseSummary } from "../release-state";
import { socialImages } from "../social";
import { DocsShell } from "./docs-shell";
import { providerStatus } from "./provider-status";
import { docsGroups, docsTopics } from "./topics";

const description = "Guides to installing xcb, connecting Claude, Codex, or Devin accounts, and sending it work from its terminal, another agent, or your own app.";

export const metadata: Metadata = {
  title: "Documentation · xcb",
  description,
  alternates: { canonical: "/docs" },
  openGraph: {
    title: "Documentation · xcb",
    description,
    type: "article",
    siteName: "xcb",
    url: "/docs",
    images: socialImages,
  },
  twitter: {
    card: "summary_large_image",
    title: "Documentation · xcb",
    description,
    images: socialImages,
  },
};

export default function Docs() {
  return (
    <>
      <DocsShell>
        <header className="xcb-docs-heading">
          <p className="xcb-docs-eyebrow">The field guide</p>
          <h1>Set up xcb and route your first task.</h1>
          <p className="xcb-docs-lead">xcb routes coding tasks across the Claude, Codex, and Devin subscriptions you already pay for. Use it as your coding agent from its terminal, or send it tasks from another agent or your own app.</p>
        </header>
        <div className="xcb-docs-note">
          <ReleaseSummary release={publishedRelease} />
          <a href="/docs/getting-started">Install xcb and send your first task →</a>
        </div>
        {docsGroups.map((group) => (
          <section aria-label={group.name} key={group.name}>
            <p className="xcb-docs-eyebrow">{group.name} · {group.reader}</p>
            <div className="xcb-docs-cards">
              {docsTopics.filter((topic) => topic.group === group.name).map((topic) => (
                <a className="xcb-docs-card hraness-material-pane" href={`/docs/${topic.slug}`} key={topic.slug}>
                  <span className="xcb-docs-card-index">{String(docsTopics.indexOf(topic) + 1).padStart(2, "0")}</span>
                  <h2>{topic.title} <span aria-hidden="true">→</span></h2>
                  <p>{topic.description}</p>
                </a>
              ))}
            </div>
          </section>
        ))}
        <article className="xcb-docs-body">
          <section aria-labelledby="readiness">
            <h2 id="readiness">What works today</h2>
            <p>{providerStatus.claude} {providerStatus.codex} {providerStatus.devin}</p>
            <p>Providers work through xcb’s file tools rather than their own shells and plugins, so a task can do less than in the provider’s own CLI. Codex, Devin, and the <a href="/docs/workspace">command runner</a> need macOS ARM64; on Linux, xcb runs Claude. See <a href="/docs/providers">accounts and models</a> for supported builds.</p>
          </section>
          <section aria-labelledby="standalone-package">
            <h2 id="standalone-package">Building on xcb?</h2>
            <p>Another agent or a script can hand xcb a task with <a href="/docs/route"><code>xcb --json route</code></a>, which picks the account and model. A TypeScript app can embed the <a href="/docs/sdk">SDK</a>, which runs tasks on the account and model the app names. For one tool-free model response per call, use the <a href="/docs/application-api">application API</a>.</p>
          </section>
        </article>
      </DocsShell>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh/docs" />
    </>
  );
}
