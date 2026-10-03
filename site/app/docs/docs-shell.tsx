import type { ReactNode } from "react";
import { SiteHeader } from "../site-header";
import { docsTopics, type DocsSlug } from "./topics";

function DocsNavigation({ active, label }: { active?: DocsSlug; label: string }) {
  return (
    <>
      <nav aria-label={label}>
        <a href="/docs" aria-current={active === undefined ? "page" : undefined}>Overview</a>
        {(["Start here", "Daily use", "Build with xcb"] as const).map((group) => (
          <div className="xcb-docs-nav-group" key={group}>
            <p>{group}</p>
            <ul>
              {docsTopics.filter((topic) => topic.group === group).map((topic) => (
                <li key={topic.slug}>
                  <a href={`/docs/${topic.slug}`} aria-current={active === topic.slug ? "page" : undefined}>
                    {topic.title}
                  </a>
                </li>
              ))}
            </ul>
          </div>
        ))}
      </nav>
      <a className="xcb-docs-source" href="https://github.com/hraness/xcb">View source on GitHub ↗</a>
    </>
  );
}

export function DocsShell({ active, children }: { active?: DocsSlug; children: ReactNode }) {
  return (
    <div className="xcb-docs" data-hraness-marketing-preset="minimal">
      <SiteHeader active="docs" />
      <div className="xcb-docs-shell">
        <aside className="xcb-docs-sidebar">
          <div className="xcb-docs-desktop-nav">
            <p className="xcb-docs-nav-title">Documentation</p>
            <DocsNavigation active={active} label="Documentation" />
          </div>
          <details className="xcb-docs-mobile-nav">
            <summary>Browse documentation</summary>
            <DocsNavigation active={active} label="Documentation menu" />
          </details>
        </aside>
        <main id="main" tabIndex={-1} className="xcb-docs-main">
          {children}
        </main>
      </div>
    </div>
  );
}
