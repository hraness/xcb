import type { ReactNode } from "react";
import { SiteHeader } from "../site-header";
import { docsTopics, type DocsSlug } from "./topics";

export function DocsShell({ active, children }: { active?: DocsSlug; children: ReactNode }) {
  return (
    <div className="xcb-docs" data-hraness-marketing-preset="editorial">
      <SiteHeader active="docs" />
      <div className="xcb-docs-shell">
        <aside className="xcb-docs-sidebar">
          <nav aria-label="Documentation">
            <p className="xcb-docs-nav-title">Documentation</p>
            <a href="/docs" aria-current={active === undefined ? "page" : undefined}>Overview</a>
            {(["Start here", "Daily use", "Build with XCB"] as const).map((group) => (
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
        </aside>
        <main id="main" tabIndex={-1} className="xcb-docs-main">
          {children}
        </main>
      </div>
    </div>
  );
}
