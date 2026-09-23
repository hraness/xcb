import type { Metadata } from "next";
import { AskAiAboutThis } from "@hraness/ui";
import { publishedRelease } from "../publication";
import { socialImages } from "../social";
import { DocsShell } from "./docs-shell";
import { docsTopics } from "./topics";

const description = "Guides to building xcb from source, connecting Claude, Codex, or Devin accounts, and calling it from the terminal, another agent, or your own app.";

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
          <p className="xcb-docs-lead">These guides take you from a source build to connected accounts, then show how to send xcb work from the terminal, another agent, or your own app.</p>
        </header>
        <p className="xcb-docs-note">{publishedRelease === null
          ? <>xcb is available from source. No native xcb release or <code>@hraness/xcb</code> npm package is published yet.</>
          : <>Latest verified release: v{publishedRelease.version}.</>} <a href="/docs/getting-started">Build and get started →</a></p>
        <div className="xcb-docs-cards">
          {docsTopics.map((topic, index) => (
            <a className="xcb-docs-card hraness-material-pane" href={`/docs/${topic.slug}`} key={topic.slug}>
              <span className="xcb-docs-card-index">0{index + 1}</span>
              <h2>{topic.title} <span aria-hidden="true">→</span></h2>
              <p>{topic.description}</p>
            </a>
          ))}
        </div>
        <article className="xcb-docs-body">
          <section aria-labelledby="readiness">
            <h2 id="readiness">What works today</h2>
            <p>Claude and Codex have each finished a real coding task on macOS ARM64 with the tested accounts and the supported builds: a failing test, a fix, a passing test, and filtered Git status. The supported Devin builds pass xcb’s sandbox checks without signing in, but a coding session on a signed-in Devin account hasn’t been confirmed. xcb checks your account’s model list when a Devin turn starts.</p>
            <p>xcb is a source preview, and its tools are narrower than the providers’ own CLIs. A model list or a passing <code>doctor</code> check doesn’t prove that a coding session will work on your machine. See <a href="/docs/providers">provider setup</a>, <a href="/docs/workspace">command limits</a>, and the <a href="/docs/reference#readiness">full readiness record</a>.</p>
          </section>
          <section aria-labelledby="standalone-package">
            <h2 id="standalone-package">Building an application?</h2>
            <p>The native <a href="/docs/application-api">application API</a> gives your app one model response per call, with no tools, hooks, or saved history. The TypeScript SDK is a separate package with its own setup requirements. Read the <a href="https://github.com/hraness/xcb/blob/main/docs/compatibility.md">compatibility reference</a> or the <a href="/docs/reference#standalone-package">package overview</a>.</p>
          </section>
        </article>
      </DocsShell>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh/docs" />
    </>
  );
}
