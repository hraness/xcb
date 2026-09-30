import { AskAiAboutThis } from "@hraness/ui";
import { MarketingComparison } from "@hraness/design-kit/react/server";
import { PageHeader, PageSection } from "../page-header";
import { SiteHeader } from "../site-header";
import { comparisonHeading, type Comparison } from "./comparisons";
import { RichText, TextLink } from "./rich-text";

const dateFormat = new Intl.DateTimeFormat("en-US", { dateStyle: "long", timeZone: "UTC" });

export function formatDate(isoDate: string): string {
  return dateFormat.format(new Date(`${isoDate}T00:00:00Z`));
}

/** The calm template every "xcb vs <tool>" page renders from its record. */
export function ComparisonPage({ entry }: Readonly<{ entry: Comparison }>) {
  return (
    <div data-hraness-marketing-preset="minimal" className="xcb-compare-page">
      <SiteHeader active="compare" />
      <main id="main" tabIndex={-1} className="xcb-page">
        <PageHeader
          id={`compare-${entry.slug}-title`}
          eyebrow={<a href="/compare">Compare</a>}
          title={comparisonHeading(entry)}
          lead={<RichText text={entry.lead} />}
          meta={<>Updated <time dateTime={entry.updated}>{formatDate(entry.updated)}</time>. Sources are listed below.</>}
        />
        <PageSection id="picks" title="Which one to pick">
          <div className="xcb-picks">
            <div>
              <h3>Pick {entry.tool} when</h3>
              <ul>{entry.picks.tool.map((line) => <li key={line}><RichText text={line} /></li>)}</ul>
            </div>
            <div>
              <h3>Pick xcb when</h3>
              <ul>{entry.picks.xcb.map((line) => <li key={line}><RichText text={line} /></li>)}</ul>
            </div>
          </div>
        </PageSection>
        <PageSection id="table" title="Side by side" wide>
          <MarketingComparison
            caption={`${entry.tool} and xcb at a glance`}
            highlight={1}
            note={entry.note === undefined ? undefined : <RichText text={entry.note} />}
            options={[{ name: entry.tool }, { name: "xcb", mark: "/marks/xcb.svg" }]}
            rows={entry.glance}
          />
          <details className="xcb-comparison-details">
            <summary>Read the full comparison</summary>
            <dl>
              {entry.rows.map((row) => <div key={row.aspect}>
                <dt>{row.aspect}</dt>
                <dd><strong>{entry.tool}</strong> <RichText text={row.tool} /></dd>
                <dd><strong>xcb</strong> <RichText text={row.xcb} /></dd>
              </div>)}
            </dl>
          </details>
        </PageSection>
        {(entry.sections ?? []).map((section) => (
          <PageSection key={section.id} id={section.id} title={section.title}>
            {section.paragraphs.map((paragraph) => <p key={paragraph}><RichText text={paragraph} /></p>)}
          </PageSection>
        ))}
        <PageSection id="sources" title="Sources">
          <ul className="xcb-source-list">
            {entry.sources.map((source) => (
              <li key={source.href}>
                <TextLink href={source.href}>{source.label}</TextLink>
                <small>Checked <time dateTime={source.checkedOn}>{formatDate(source.checkedOn)}</time></small>
              </li>
            ))}
          </ul>
        </PageSection>
        <nav className="xcb-page-footer-nav" aria-label="More comparisons">
          <a href="/compare">All comparisons →</a>
          <a href="/install">Install xcb →</a>
        </nav>
      </main>
      <AskAiAboutThis className="ask-ai" url={`https://xcb.sh/compare/${entry.slug}`} />
    </div>
  );
}
