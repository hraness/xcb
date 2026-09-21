import type { Metadata } from "next";
import { AskAiAboutThis } from "@hraness/ui";
import { DocsShell } from "./docs-shell";
import { docsTopics } from "./topics";

export const metadata: Metadata = {
  title: "Documentation · XCB",
  description: "Install XCB, connect your AI subscriptions, work in the terminal, and build applications with a qualified local inference interface.",
  alternates: { canonical: "/docs" },
  openGraph: {
    title: "Documentation · XCB",
    description: "Practical guides to accounts, models, isolated workspace commands, customization, and the application API.",
    type: "article",
    siteName: "xcb",
    url: "/docs",
  },
  twitter: {
    card: "summary_large_image",
    title: "Documentation · XCB",
    description: "Get started with XCB and find the guide for your next step.",
  },
};

export default function Docs() {
  return (
    <>
      <DocsShell>
        <header className="xcb-docs-heading">
          <p className="xcb-docs-eyebrow">The field guide</p>
          <h1>Start here. Make it yours.</h1>
          <p className="xcb-docs-lead">One terminal workspace for your coding accounts, sessions, and tools. These guides take you from a source build to a working setup.</p>
        </header>
        <p className="xcb-docs-note">XCB is available from source. No native XCB release or <code>@hraness/xcb</code> npm package is published yet. <a href="/docs/getting-started">Build and get started →</a></p>
        <div className="xcb-docs-cards">
          {docsTopics.map((topic, index) => (
            <a className="xcb-docs-card hraness-material-pane" href={`/docs/${topic.slug}`} key={topic.slug}>
              <span className="xcb-docs-card-index">0{index + 1}</span>
              <h2>{topic.title} <span aria-hidden="true">↗</span></h2>
              <p>{topic.description}</p>
            </a>
          ))}
        </div>
        <article className="xcb-docs-body">
          <section aria-labelledby="readiness">
            <h2 id="readiness">What works today</h2>
            <p>Installed Claude and Codex coding workflows have passed on macOS ARM64 with the tested accounts and admitted builds. That includes a failing test, a repair, a passing test, and filtered Git status. Devin model discovery passed, but provider quota blocked its coding acceptance.</p>
            <p>XCB is still a source preview, with a narrower tool boundary than the providers’ own CLIs. A model listing or successful <code>doctor</code> check is not proof of a working session on your host. See <a href="/docs/providers">provider setup</a>, <a href="/docs/workspace">command limits</a>, and the <a href="/docs/reference#readiness">full readiness record</a>.</p>
          </section>
          <section aria-labelledby="standalone-package">
            <h2 id="standalone-package">Building an application?</h2>
            <p>The native <a href="/docs/application-api">application API</a> gives a host bounded, ephemeral inference with no tools or hooks. The retained TypeScript SDK is a separate compatibility surface with its own admission requirements. Read the <a href="https://github.com/hraness/xcb/blob/main/docs/compatibility.md">compatibility reference</a> or the <a href="/docs/reference#standalone-package">package overview</a>.</p>
          </section>
        </article>
      </DocsShell>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh/docs" />
    </>
  );
}
