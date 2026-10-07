import {
  MarketingInterfaceGrid,
  MarketingPage,
  MarketingPillars,
  MarketingQuestionList,
  MarketingSection,
  PlatformBadges,
  ProductHero,
  ProviderMark,
} from "@hraness/design-kit/react/server";
import { PlatformInstall } from "@hraness/design-kit/react";
import { AskAiAboutThis } from "@hraness/ui";
import { CodeBlock } from "./code-block";
import { FounderNote } from "./founder-note";
import { RouterShowcase } from "./mockups/showcase";
import "./mockups/mockups.css";
import { providerStatus } from "./docs/provider-status";
import { installPlatforms, runsOnPlatforms } from "./install/platforms";
import { publishedRelease } from "./publication";
import { releaseStatusLabel } from "./release-state";
import { SiteHeader } from "./site-header";

import { productMessaging, productName } from "./messaging";

const repository = "https://github.com/hraness/xcb";

// Hero heading and summary come from the portfolio registry's xcb messaging record.
const heading = productMessaging.hero.heading;
const summary = productMessaging.hero.summary;
const metaDescription = productMessaging.meta;

const questions = [
  { question: "Can I use the subscriptions I already have?", answer: "Yes. You sign in to your own Claude and Codex accounts through each provider’s own tool, and xcb routes work among them. It has no model access of its own and does not lift provider usage limits; each provider’s pricing and terms still apply." },
  { question: "What happens when an account hits its limit?", answer: "When a provider reports an exhausted usage window for an account, xcb skips that account and sends the next task to another account that can take it. A managed task that stops on a reported limit can move to another account with its original instructions. If no account can take it, the task waits for the reset. xcb never falls back to an API key." },
  { question: "How is xcb different from claude-swap, Claude Code Router, or herdr?", answer: "claude-swap changes which login Claude Code uses and keeps every Claude Code feature; xcb picks an account for each task across Claude and Codex and sandboxes each run. Claude Code Router sends each API request to a provider you configure; xcb never touches API traffic and runs whole tasks on your subscriptions. herdr keeps many agent terminals alive and visible, and xcb can run inside a herdr pane. The comparison pages cover these and more." },
  { question: "What stays on my computer?", answer: "xcb stores accounts, credentials, sessions, and settings on your computer, outside your projects. Model requests go to the provider that runs the task. The optional judge sends limited task context to Cloudflare’s Clef service; the current source build has no hosted remote commands. Valhalla transport is planned, not shipped. The usage history the installer turns on is kept by aicharts on your computer and never uploaded; it checks GitHub once a day for its own verified update." },
  { question: "Can xcb run my tests and builds?", answer: "On macOS with Apple silicon, commands run offline in a Linux VM with public dependencies you prepare in advance. Git is read-only there, so you review and commit the changes yourself. Native macOS builds cannot run. Registered host MCP tools can provide additional capabilities; native provider shells remain unavailable." },
  { question: "What does it cost?", answer: "xcb is free and MIT licensed. You pay only for your provider subscriptions and any services you choose to use." },
] as const;

const agentExample = 'xcb run -p "Fix the failing parser test"';

const routeExample = `$ xcb --json route < task.json`;

const importExample = `xcb sessions discover
xcb sessions import --recent`;

const readiness = [
  { name: "Claude", mark: "claudecode", detail: providerStatus.claude },
  { name: "Codex", mark: "codex", detail: providerStatus.codex },
] as const;

export default function Home() {
  const publisher = { "@type": "Organization", "@id": "https://hraness.com/#organization", name: "Hraness", url: "https://hraness.com/" };
  const structuredData = [
    { "@context": "https://schema.org", "@type": "WebSite", "@id": "https://xcb.sh/#website", url: "https://xcb.sh/", name: productName, alternateName: ["xcb", "Excalibur"], inLanguage: "en-US", publisher },
    {
      "@context": "https://schema.org",
      "@type": "SoftwareApplication",
      "@id": "https://xcb.sh/#app",
      name: productName,
      alternateName: ["xcb", "Excalibur"],
      url: "https://xcb.sh/",
      description: metaDescription,
      applicationCategory: "DeveloperApplication",
      operatingSystem: "macOS, Linux, Windows",
      installUrl: "https://xcb.sh/install",
      isAccessibleForFree: true,
      offers: { "@type": "Offer", price: 0, priceCurrency: "USD" },
      license: "https://opensource.org/license/mit",
      publisher,
      ...(publishedRelease === null ? {} : { softwareVersion: publishedRelease.version }),
    },
    { "@context": "https://schema.org", "@type": "SoftwareSourceCode", name: productName, description: metaDescription, codeRepository: repository, programmingLanguage: ["Rust", "TypeScript"], license: "https://opensource.org/license/mit", url: "https://xcb.sh/", targetProduct: { "@id": "https://xcb.sh/#app" } },
    { "@context": "https://schema.org", "@type": "FAQPage", mainEntity: questions.map(({ question, answer }) => ({ "@type": "Question", name: question, acceptedAnswer: { "@type": "Answer", text: answer } })) },
  ];
  return (
    <div data-hraness-marketing-preset="minimal" className="xcb-home">
      <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: JSON.stringify(structuredData) }} />
      <SiteHeader active="home" />
      <main id="main" tabIndex={-1}>
        <MarketingPage>
          <ProductHero
            align="start"
            backdrop={false}
            className="xcb-hero"
            frame={<RouterShowcase className="xcb-home-showcase" />}
            name=""
            heading={heading}
            headingId="hero-title"
            summary={summary}
            install={publishedRelease === null
              ? <CodeBlock code={"git clone https://github.com/hraness/xcb.git && cd xcb\n./scripts/install-native.sh"} copyLabel="Copy commands" />
              : <PlatformInstall id="hero-install" platforms={installPlatforms(publishedRelease)} />}
            actions={[{ href: "/install", label: "Install guide", emphasis: "secondary" }, { href: "#use", label: productMessaging.hero.secondaryAction, emphasis: "secondary" }]}
            boundary={`Free and open source · ${releaseStatusLabel(publishedRelease)}`}
          />

          <FounderNote
            emoji="⚔️"
            paragraphs={[
              "Excalibur (xcb) is a tool for operating AI subscriptions. Just log in with all your Codex and Claude accounts, then tell your agent to use xcb. Personally, I like asking my agent to create long-running jobs that scale parallelism based on load and available usage. xcb exposes pretty much every feature in Claude Code and Codex, so you can create whatever custom setup you want (you can even build apps on top of it). I’m going to China for a month, and I plan to leave several herds of agents running on one of my laptops, shepherded by xcb.",
            ]}
            action={{ label: "Tell your agent to set it up:", href: "https://xcb.sh" }}
            signature="Ben Guo"
          />

          <p className="xcb-launch-link"><a className="xcb-text-link" href="/blog/one-agent-for-all-your-ai-plans">Introducing Excalibur: the short version →</a></p>

          <MarketingSection id="router" heading={productMessaging.headings["home-router"]} headingId="router-title" summary="Let xcb choose among your subscriptions when you send a task. It favors unused Claude and Codex quota approaching a reset when fresh usage reports are available.">
            <MarketingPillars
              ariaLabel="How xcb uses your subscriptions"
              columns={3}
              presentation="benefits"
              pillars={[
                { label: "Keep work moving", summary: "When an account reaches a known limit, xcb can continue your task on another account that has room. If none can take it, the task waits for a reset." },
                { label: "Use the capacity you have", summary: "Give work to accounts with quota to spare when their next reset is close. xcb takes weekly limits into account too." },
                { label: "Keep the model your task needs", summary: "Quota timing works within the task’s quality requirements. Your choice of provider, account, or model comes first." },
              ]}
            />
            <a className="xcb-text-link" href="/docs/how-routing-works">How routing works →</a>
          </MarketingSection>

          <MarketingInterfaceGrid
            id="use"
            heading={productMessaging.headings["home-interfaces"]}
            headingId="use-title"
            interfaces={[
              {
                label: productMessaging.headings["home-interface-agent"],
                summary: "Send work through the headless CLI. xcb picks an account and model and prints the result; managed backlog tasks can outlive their caller.",
                example: <><CodeBlock code={agentExample} copyLabel="Copy command" /><p>Run this command in your project folder.</p><a className="xcb-text-link" href="/docs/getting-started">Getting started →</a></>,
              },
              {
                label: productMessaging.headings["home-interface-integration"],
                summary: "Keep your existing agent or app and hand a task to xcb. It picks an account and model, runs the task, and returns the result as JSON.",
                example: <><CodeBlock code={routeExample} copyValue="xcb --json route < task.json" copyLabel="Copy command" /><p>With the <a href="/docs/sdk">TypeScript SDK</a>, the app names the account and model.</p><a className="xcb-text-link" href="/docs/route">Route tasks →</a></>,
              },
            ]}
          />

          <MarketingSection id="import-sessions" heading={productMessaging.headings["home-import-sessions"]} headingId="import-sessions-title" summary="Import conversations used in the last 24 hours by default, then continue with an account that can take the task.">
            <CodeBlock code={importExample} copyLabel="Copy commands" />
            <p>xcb copies user and assistant messages as context. List imports with <code>xcb conversations --json</code> and submit a managed task with <code>xcb backlog add</code> to continue. Source files stay in place, and import does not take control of a running provider session.</p>
            <a className="xcb-text-link" href="/docs/projects-and-tasks#import-sessions">Import conversations →</a>
          </MarketingSection>

          <MarketingSection id="usage" heading="See what your agents used" headingId="usage-title" summary="The installer adds aicharts beside xcb and turns on its local usage history: your token use by day, agent, provider and model, kept on this computer and never uploaded.">
            <CodeBlock code={"xcb usage\nxcb usage report --csv\nxcb usage connect"} copyLabel="Copy commands" />
            <p><code>xcb usage connect</code> gives Claude Code and Codex the same read-only tools through aicharts’ MCP server, so an agent you route can answer “what did I spend this week” itself. aicharts also checks GitHub once a day for a new release and installs it only after verifying it; <code>aicharts update disable</code> turns that off. <a className="xcb-text-link" href="https://aicharts.io/usage">aicharts usage →</a></p>
          </MarketingSection>

          <MarketingSection id="install" heading={productMessaging.headings["home-install"]} headingId="install-title" summary={publishedRelease === null ? "No release is published yet; build from source with Git and Rust 1.97.1." : "Install the latest release, connect your Claude account, and run a task."}>
            {publishedRelease === null
              ? <p>Build xcb with the command above, then follow the <a href="/install">installation guide</a> to connect your first provider.</p>
              : (
                <>
                  <PlatformBadges platforms={runsOnPlatforms(publishedRelease)} />
                  <p>Then connect your Claude account and run a task. On Linux, Claude needs the <a href="/docs/providers#claude-on-linux">sandbox setup</a> before your first task. On Windows, Claude runs in WSL2.</p>
                  <CodeBlock code={'xcb setup claude\nxcb run -p "Explain this repository"'} copyLabel="Copy commands" />
                </>
              )}
            <p className="xcb-install-links"><a className="xcb-text-link" href="/install">Install guide →</a></p>
          </MarketingSection>

          <details className="xcb-provider-details" id="readiness">
            <summary>Provider support and tested builds</summary>
            <dl className="xcb-status-list">
              {readiness.map((row) => (
                <div key={row.name}>
                  <dt><span aria-hidden="true"><ProviderMark mark={row.mark} label={row.name} size={20} /></span>{row.name}</dt>
                  <dd>{row.detail}</dd>
                </div>
              ))}
            </dl>
            <a className="xcb-text-link" href="/docs/providers">Supported builds and setup →</a>
          </details>

          <MarketingQuestionList heading={productMessaging.headings["home-questions"]} headingId="questions-title" id="questions" questions={questions.map(({ question, answer }) => ({ question, answer: <p>{answer}</p> }))} />
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh" />
    </div>
  );
}
