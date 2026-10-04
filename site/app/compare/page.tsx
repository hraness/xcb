import type { Metadata } from "next";
import { AskAiAboutThis } from "@hraness/ui";
import { PageHeader, PageSection } from "../page-header";
import { SiteHeader } from "../site-header";
import { formatDate } from "./comparison-page";
import { comparisonHeading, comparisonPath, comparisons } from "./comparisons";
import { hubDescription, hubElsewhere, hubGroups, hubHeading, hubLead, hubTitle, hubUpdated } from "./hub";
import { RichText, TextLink } from "./rich-text";

export const metadata: Metadata = {
  title: hubTitle,
  description: hubDescription,
  alternates: { canonical: "/compare" },
  openGraph: { title: hubTitle, description: hubDescription, siteName: "Excalibur (xcb)", type: "website", url: "/compare" },
  twitter: { card: "summary_large_image", title: hubTitle, description: hubDescription },
};

export default function Compare() {
  return (
    <div data-hraness-marketing-preset="minimal" className="xcb-compare-page">
      <SiteHeader active="compare" />
      <main id="main" tabIndex={-1} className="xcb-page">
        <PageHeader
          id="compare-title"
          title={hubHeading}
          lead={<RichText text={hubLead} />}
          meta={<>Updated <time dateTime={hubUpdated}>{formatDate(hubUpdated)}</time>. Each tool is described from its own site or documentation.</>}
        />
        <PageSection id="pages" title="Side-by-side pages">
          <ul className="xcb-source-list">
            {comparisons.map((entry) => (
              <li key={entry.slug}>
                <a href={comparisonPath(entry.slug)}>{comparisonHeading(entry)}</a>
                <small><RichText text={entry.description} /></small>
              </li>
            ))}
          </ul>
        </PageSection>
        {hubGroups.map((group) => (
          <PageSection key={group.id} id={group.id} title={group.title}>
            <p><RichText text={group.summary} /></p>
            <ul className="xcb-source-list">
              {group.tools.map((tool) => (
                <li key={tool.name}>
                  <TextLink href={tool.href}>{tool.name}</TextLink>
                  <small>{tool.summary}</small>
                </li>
              ))}
            </ul>
          </PageSection>
        ))}
        <PageSection id="elsewhere" title="When another tool fits better">
          <ul>{hubElsewhere.map((line) => <li key={line}><RichText text={line} /></li>)}</ul>
        </PageSection>
        <nav className="xcb-page-footer-nav" aria-label="Next steps">
          <a href="/install">Install xcb →</a>
          <a href="/docs/getting-started">Getting started →</a>
        </nav>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh/compare" />
    </div>
  );
}
