import {
  MarketingInterfaceGrid,
  MarketingPage,
  MarketingPillars,
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
import { PressureFigure } from "./figures/pressure-figure";
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

// Hero copy comes from the portfolio registry's xcb messaging record.
const heading = productMessaging.hero.heading;
const summary = productMessaging.hero.summary;
const metaDescription = productMessaging.meta;

const questions = [
  { question: "Can I use the subscriptions I already have?", answer: "Yes. You sign in to your own Claude and Codex accounts through each provider’s own tool, and xcb routes work among them. It has no model access of its own and does not lift provider usage limits; each provider’s pricing and terms still apply." },
  { question: "What happens when an account hits its limit?", answer: "When a provider reports an exhausted usage window for an account, xcb skips that account and sends the next task to another account that can take it. A managed task that stops on a reported limit can move to another account with its original instructions. If no account can take it, the task waits for the reset. xcb never falls back to an API key." },
  { question: "Is there an app or a terminal UI?", answer: "No. xcb is a command-line control plane built for unattended use. You sign in once, then an agent creates projects, tasks, schedules, routes, and hooks through xcb’s commands, and you check in with xcb tasks and xcb attention. Any terminal multiplexer or agent terminal, including herdr, can hold the agent that drives it." },
  { question: "How is xcb different from claude-swap, Claude Code Router, or an LLM gateway?", answer: "claude-swap changes which login Claude Code uses and keeps every Claude Code feature; xcb picks an account for each task across Claude and Codex and sandboxes each run. Claude Code Router and LLM gateways such as OpenRouter route API requests per token; xcb never touches API traffic and runs whole tasks on the subscriptions you already pay for. The comparison pages cover these and more." },
  { question: "What stays on my computer?", answer: "xcb stores accounts, credentials, sessions, and settings on your computer, outside your projects. Model requests go to the provider that runs the task. The optional judge sends limited task context to Cloudflare’s Clef service, and xcb has no hosted remote commands. The usage history the installer turns on is kept by aicharts on your computer and never uploaded; it checks GitHub once a day for its own verified update." },
  { question: "Can xcb run my tests and builds?", answer: "On macOS with Apple silicon, commands run offline in a Linux VM with public dependencies you prepare in advance. Git is read-only there, so you review and commit the changes yourself. Native macOS builds cannot run. Registered host MCP tools can provide additional capabilities; native provider shells remain unavailable." },
  { question: "What does it cost?", answer: "xcb is free and MIT licensed. You pay only for your provider subscriptions and any services you choose to use." },
] as const;

const agentExample = 'xcb run -p "Fix the failing parser test"';

const routeExample = `$ xcb --json route < task.json`;

const topologyExample = `xcb backlog add /absolute/path/to/project "Fix the failing parser test" --ready
xcb schedules add /absolute/path/to/project "Review previous work" --every 3600
xcb tasks
xcb attention`;

const importExample = `xcb sessions discover
xcb sessions import --recent`;

const readiness = [
  { name: "Claude", mark: "claudecode", detail: providerStatus.claude },
  { name: "Codex", mark: "codex", detail: providerStatus.codex },
] as const;

/**
 * Figures from the owner's post of 12 October 2026 about one month of
 * unattended work across Hraness's own projects. They describe that month,
 * not xcb's performance, and the source line says so.
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
            frame={<RouteProof />}
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

          <MarketingSection id="router" heading={productMessaging.headings["home-router"]} headingId="router-title" summary="Every task goes to one account that can take it now. xcb reads each account’s usage, favors unused quota that is about to reset, and moves on when a provider reports a limit.">
            <RoutingFigure />
            <MarketingPillars
              ariaLabel="How xcb picks an account"
              columns={3}
              presentation="benefits"
              pillars={[
                { label: "Filter to accounts that can take it", summary: "An account qualifies when it runs a supported build, is signed in, is idle, is not at a known quota limit, and has a recently seen model." },
                { label: "Rank by quota about to reset", summary: "Usage reports no more than five minutes old decide the order. Unused quota approaching a reset goes first, weekly limits included." },
                { label: "Hold the account, record the task", summary: "While a task runs, xcb holds the account so no other task can use it. The task’s local record can be replayed with xcb tasks verify." },
              ]}
            />
            <a className="xcb-text-link" href="/docs/how-routing-works">How routing works →</a>
          </MarketingSection>

          <MarketingSection id="primitives" heading="Ask an agent for any topology" headingId="primitives-title" summary="There is no app or TUI. Sign in once, describe the setup you want, and the agent builds it from xcb’s primitives. Agents coordinate through a backlog and an attention queue, so you check in instead of watching.">
            <MechanismDiagram />
            <CodeBlock code={topologyExample} copyLabel="Copy commands" />
            <p>Backlog tasks run in xcb’s background supervisor and outlive the terminal that created them. <code>xcb tasks</code> lists them across projects, and <code>xcb attention</code> collects the questions and approvals waiting on you.</p>
            <a className="xcb-text-link" href="/docs/projects-and-tasks">Projects, tasks, and schedules →</a>
          </MarketingSection>

          <MarketingSection id="scale" heading="Scale the pool with pressure" headingId="scale-title" summary="Treat your subscriptions as one budget of intelligence. xcb measures memory, disk, swap, and quota, and starts or defers workers so the pool runs as hard as the host and the accounts allow.">
            <PressureFigure />
            <a className="xcb-text-link" href={`${repository}/blob/main/docs/unattended.md`}>Unattended operation ↗</a>
          </MarketingSection>

          <MarketingSection id="idea" heading="Built for weeks of unattended work" headingId="idea-title" summary="Hraness runs its own projects on xcb. The owner’s post of 12 October 2026 describes the setup and the month that led to it.">
            <blockquote className="xcb-idea">
              <p>“I have an intelligence budget that I scale up or down from month to month. I assign my pool of intelligence to a set of projects, and I let it cook for weeks unattended.”</p>
              <cite>From the Hraness post of 12 October 2026</cite>
            </blockquote>
            <MarketingStatStrip
              ariaLabel="One month of unattended work at Hraness"
              columns={4}
              stats={factoryStats}
              source="Figures from the same post, describing one month across Hraness’s own projects and accounts. They describe that setup, not what xcb does for every pool."
            />
          </MarketingSection>

          <MarketingInterfaceGrid
            id="use"
            heading={productMessaging.headings["home-interfaces"]}
            headingId="use-title"
            interfaces={[
              {
                label: productMessaging.headings["home-interface-agent"],
                summary: "Send work through the command line. xcb picks an account and model and prints the result; managed backlog tasks can outlive their caller.",
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
            <p><code>xcb usage connect</code> gives Claude Code and Codex the same read-only tools through aicharts’ MCP server, so an agent you route can answer “what did I spend this week” itself. aicharts also checks GitHub once a day for a new release and installs it only after verifying it; <code>aicharts update disable</code> turns that off. <a className="xcb-text-link" href="https://aicharts.io/usage">aicharts usage ↗</a></p>
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
