import {
  MarketingInterfaceGrid,
  MarketingPage,
  MarketingQuestionList,
  MarketingSection,
  MarketingStatStrip,
  PlatformBadges,
  ProductHero,
  ProviderMark,
} from "@hraness/design-kit/react/server";
import { PlatformInstall } from "@hraness/design-kit/react";
import { AskAiAboutThis } from "@hraness/ui";
import { CodeBlock } from "./code-block";
import { MechanismDiagram } from "./figures/mechanism-diagram";
import { PoolPanel } from "./figures/pool-panel";
import { RouteProof } from "./figures/route-proof";
import { RoutingFigure } from "./figures/routing-figure";
import "./figures/figures.css";
import { providerStatus } from "./docs/provider-status";
import { installPlatforms, runsOnPlatforms } from "./install/platforms";
import { publishedRelease } from "./publication";
import { releaseStatusLabel } from "./release-state";
import { SiteHeader } from "./site-header";

import { productMessaging, productName } from "./messaging";

const repository = "https://github.com/hraness/xcb";
const ownerPost = "https://hraness.substack.com/p/a-love-letter-to-typing-code-by-hand";

// Hero copy comes from the portfolio registry's xcb messaging record.
const heading = productMessaging.hero.heading;
const summary = productMessaging.hero.summary;
const metaDescription = productMessaging.meta;

const questions = [
  { question: "Can I use the subscriptions I already have?", answer: "Yes. You sign in to your own Claude and Codex accounts through each provider’s own tool, and xcb routes work among them. It has no model access of its own and does not lift provider usage limits; each provider’s pricing and terms still apply." },
  { question: "What happens when an account hits its limit?", answer: "When a provider reports an exhausted usage window, xcb skips that account and sends the next task to one that can take it. A managed task that stops on a reported limit moves to another account with its original instructions. If no account can take it, the task waits for the reset. xcb never falls back to an API key." },
  { question: "Is there an app or a terminal UI?", answer: "No. xcb is a command-line control plane built for unattended use. You sign in once, then an agent creates projects, tasks, schedules, routes, and hooks through xcb’s commands, and you check in with xcb tasks and xcb attention. Any agent terminal, including herdr, can hold the agent that drives it." },
  { question: "How is xcb different from claude-swap, Claude Code Router, or an LLM gateway?", answer: "claude-swap changes which login Claude Code uses; xcb picks an account for each task across Claude and Codex and sandboxes each run. Claude Code Router and LLM gateways such as OpenRouter route API requests per token; xcb never touches API traffic and runs whole tasks on the subscriptions you already pay for." },
  { question: "What stays on my computer?", answer: "xcb stores accounts, credentials, sessions, and settings on your computer, outside your projects. Model requests go to the provider that runs the task. The optional judge sends limited task context to Cloudflare’s Clef service. The usage history the installer turns on is kept by aicharts on your computer and never uploaded." },
  { question: "Can xcb run my tests and builds?", answer: "On macOS with Apple silicon, commands run offline in a Linux VM with public dependencies you prepare in advance. Git is read-only there, so you review and commit the changes yourself. Native macOS builds cannot run. Registered host MCP tools can provide additional capabilities; native provider shells remain unavailable." },
] as const;

const agentExample = 'xcb run -p "Fix the failing parser test"';

const importExample = `xcb sessions discover
xcb sessions import --recent`;

const usageExample = `xcb usage
xcb usage report --csv
xcb usage connect`;

const readiness = [
  { name: "Claude", mark: "claudecode", detail: providerStatus.claude },
  { name: "Codex", mark: "codex", detail: providerStatus.codex },
] as const;

/**
 * Figures from the owner's post of 12 October 2026 (hraness.substack.com/p/a-love-letter-to-typing-code-by-hand)
 * about one month of unattended work across Hraness's own projects. The strip's
 * source line names the post.
 */
const factoryStats = [
  { label: "Subscriptions", value: "30", detail: "Codex, Claude, and other plans in one pool" },
  { label: "Commits in a month", value: "7,927", detail: "an average of 264 a day" },
  { label: "Lines changed", value: "17.6M", detail: "across the projects in that month" },
  { label: "Tokens in a month", value: "190B", detail: "peaking at 300M tokens an hour" },
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
            className="xcb-hero"
            eyebrow={productMessaging.category}
            frame={<PoolPanel />}
            layout="split"
            name=""
            heading={heading}
            headingId="hero-title"
            summary={summary}
            install={publishedRelease === null
              ? <CodeBlock code={"git clone https://github.com/hraness/xcb.git && cd xcb\n./scripts/install-native.sh"} copyLabel="Copy commands" />
              : <PlatformInstall id="hero-install" platforms={installPlatforms(publishedRelease)} />}
            actions={[{ href: "/install", label: productMessaging.hero.primaryAction, emphasis: "primary" }, { href: "#router", label: productMessaging.hero.secondaryAction, emphasis: "secondary" }]}
            boundary={`Free and open source · ${releaseStatusLabel(publishedRelease)}`}
          />

          <MarketingSection id="router" label="Routing" heading={productMessaging.headings["home-router"]} headingId="router-title" summary="Each task goes to one account that can take it now, favoring unused quota that is about to reset.">
            <RoutingFigure />
            <a className="xcb-text-link" href="/docs/how-routing-works">How routing works →</a>
          </MarketingSection>

          <MarketingSection id="primitives" label="Topology" heading="Ask an agent for any topology" headingId="primitives-title" summary="There is no app or TUI. Sign in once, describe the setup you want, and the agent builds it from xcb’s primitives.">
            <MechanismDiagram />
            <a className="xcb-text-link" href="/docs/projects-and-tasks">Projects, tasks, and schedules →</a>
          </MarketingSection>

          <MarketingInterfaceGrid
            id="use"
            label="Interfaces"
            heading={productMessaging.headings["home-interfaces"]}
            headingId="use-title"
            interfaces={[
              {
                label: productMessaging.headings["home-interface-agent"],
                summary: "Send work from the command line. xcb picks an account and model and prints the result.",
                example: <><CodeBlock code={agentExample} copyLabel="Copy command" /><a className="xcb-text-link" href="/docs/getting-started">Getting started →</a></>,
              },
              {
                label: productMessaging.headings["home-interface-integration"],
                summary: "Hand a task to xcb as JSON and get the result back as JSON. With the TypeScript SDK, the app names the account and model.",
                example: <><RouteProof /><a className="xcb-text-link" href="/docs/route">Route tasks →</a></>,
              },
            ]}
          />

          <MarketingSection id="idea" label="In production" heading="Built for weeks of unattended work" headingId="idea-title" summary="Hraness runs its own projects on xcb. The owner’s post of 12 October 2026 describes one month of it.">
            <MarketingStatStrip
              ariaLabel="One month of unattended work at Hraness"
              columns={4}
              stats={factoryStats}
              source={<>Figures from <a className="xcb-text-link" href={ownerPost} rel="noopener">A love letter to typing code by hand ↗</a>, one month across Hraness’s own projects and accounts.</>}
            />
          </MarketingSection>

          <MarketingSection id="import-sessions" label="Conversations" heading={productMessaging.headings["home-import-sessions"]} headingId="import-sessions-title" summary="Import conversations from the last 24 hours, then continue each one on an account that can take it.">
            <CodeBlock code={importExample} copyLabel="Copy commands" />
            <a className="xcb-text-link" href="/docs/projects-and-tasks#import-sessions">Import conversations →</a>
          </MarketingSection>

          <MarketingSection id="usage" label="Usage" heading="See what your agents used" headingId="usage-title" summary="The installer adds aicharts beside xcb, which keeps your token use by day, agent, provider, and model on this computer.">
            <CodeBlock code={usageExample} copyLabel="Copy commands" />
            <a className="xcb-text-link" href="https://aicharts.io/usage">aicharts usage ↗</a>
          </MarketingSection>

          <MarketingSection id="install" label="Install" heading={productMessaging.headings["home-install"]} headingId="install-title" summary={publishedRelease === null ? "No release is published yet; build from source with Git and Rust 1.97.1." : "Install the latest release, connect your Claude account, and run a task."}>
            {publishedRelease === null
              ? <p>Build xcb with the command above, then follow the <a href="/install">installation guide</a> to connect your first provider.</p>
              : (
                <>
                  <PlatformBadges platforms={runsOnPlatforms(publishedRelease)} />
                  <CodeBlock code={'xcb setup claude\nxcb run -p "Explain this repository"'} copyLabel="Copy commands" />
                </>
              )}
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
              <p>On Linux, Claude needs the <a href="/docs/providers#claude-on-linux">sandbox setup</a> before your first task. On Windows, Claude runs in WSL2.</p>
              <a className="xcb-text-link" href="/docs/providers">Supported builds and setup →</a>
            </details>
            <p className="xcb-install-links"><a className="xcb-text-link" href="/install">Install guide →</a></p>
          </MarketingSection>

          <MarketingQuestionList heading={productMessaging.headings["home-questions"]} headingId="questions-title" id="questions" questions={questions.map(({ question, answer }) => ({ question, answer: <p>{answer}</p> }))} />
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh" />
    </div>
  );
}
