import {
  MarketingInterfaceGrid,
  MarketingPage,
  MarketingPillars,
  MarketingQuestionList,
  MarketingSection,
  ProductHero,
  ProviderMark,
} from "@hraness/design-kit/react/server";
import { AskAiAboutThis } from "@hraness/ui";
import { CliProof } from "./cli-proof";
import { CodeBlock } from "./code-block";
import { providerStatus } from "./docs/provider-status";
import { installCommand } from "./install/commands";
import { publishedRelease } from "./publication";
import { releaseStatusLabel } from "./release-state";
import { SiteHeader } from "./site-header";

const repository = "https://github.com/hraness/xcb";

// Hero heading and summary come from the portfolio registry's xcb messaging record.
const heading = "Use your Claude, Codex, and Devin plans from one agent.";
const summary = "Work in xcb’s terminal or call it from your own agent or app. Each task runs through the provider’s own tool on one of your accounts that is signed in, idle, and not at a known limit.";
const metaDescription = "xcb routes coding tasks across the Claude, Codex, and Devin subscriptions you already pay for, picking an account that is signed in and idle.";

const questions = [
  { question: "Can I use the subscriptions I already have?", answer: "Yes. You sign in to your own Claude, Codex, and Devin accounts through each provider’s own tool, and xcb routes work among them. It has no model access of its own and does not lift provider usage limits; each provider’s pricing and terms still apply." },
  { question: "What happens when an account hits its limit?", answer: "While a provider reports a usage window for an account, xcb skips that account and sends the next task to another account that can take it. A task from your thread that stops on a reported limit can move to another account with its original instructions. If no account can take it, the task waits for the reset. xcb never falls back to an API key." },
  { question: "How is xcb different from herdr, pi, or Conductor?", answer: "herdr keeps many agent terminals alive and visible; xcb decides which of your accounts runs each task and holds that account until the run ends, so the two work together. pi is a coding agent you rebuild with extensions; xcb runs the providers’ own agents and lets you reshape everything above a fixed permission boundary. Conductor gives parallel agents their own branches; xcb sandboxes each run and keeps tasks going after you close the terminal. The comparison pages cover each tool." },
  { question: "What stays on my computer?", answer: "Accounts, credentials, sessions, and settings stay on your computer, outside your projects. Model requests go to the provider that runs the task. The optional judge, which is off by default, sends limited task context to TypeSafe’s System One service." },
  { question: "Is the self-tuning harness ready?", answer: "No. The managed harness is in development, and the current build does not run self-modifying routing policies. What learns today are two small reflexes: one picks a model tier for a new task and one notices when a task stopped early. You can inspect and roll back both." },
  { question: "What does it cost?", answer: "xcb is free and MIT licensed. You pay only for your provider subscriptions and any services you choose to use." },
] as const;

const agentExample = `$ xcb
> Fix the failing parser test in ~/src/app`;

const routeExample = `$ xcb --json route < task.json`;

const readiness = [
  { name: "Claude", mark: "claudecode", detail: providerStatus.claude },
  { name: "Codex", mark: "codex", detail: providerStatus.codex },
  { name: "Devin", mark: "devin", detail: providerStatus.devin },
  { name: "Tests and builds", mark: null, detail: "Commands run offline in a Linux VM on macOS on Apple silicon, with public dependencies you prepare in advance. Git is read-only there, so you review and commit the changes yourself." },
] as const;

export default function Home() {
  const structuredData = [
    { "@context": "https://schema.org", "@type": "SoftwareSourceCode", name: "xcb", description: metaDescription, codeRepository: repository, programmingLanguage: ["Rust", "TypeScript"], license: "https://opensource.org/license/mit", url: "https://xcb.sh" },
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
            frame={<CliProof />}
            name="xcb"
            heading={heading}
            headingId="hero-title"
            summary={summary}
            actions={[{ href: "/install", label: "Install xcb" }, { href: repository, label: "View the source ↗" }]}
            boundary={`Free and open source · macOS on Apple silicon and Linux x86_64 · ${releaseStatusLabel(publishedRelease)}`}
          />

          <MarketingInterfaceGrid
            id="use"
            heading="Two ways to use xcb"
            headingId="use-title"
            interfaces={[
              {
                label: "As your coding agent",
                summary: "Type work into one thread that spans your projects. xcb picks the project and an account that can take the task, and the task keeps running after you close the terminal.",
                example: <><CodeBlock code={agentExample} language="text" /><a className="xcb-text-link" href="/docs/getting-started">Getting started →</a></>,
              },
              {
                label: "Inside your agent or app",
                summary: "Your agent sends one JSON task to xcb --json route and gets back the result, the account and model xcb picked, and a session it can resume. Apps can embed the TypeScript SDK instead, where the app names the account and model.",
                example: <><CodeBlock code={routeExample} /><a className="xcb-text-link" href="/docs/route">Route tasks →</a></>,
              },
            ]}
          />

          <MarketingSection id="router" heading="How xcb picks an account" headingId="router-title" summary="It filters your accounts, ranks what is left, and holds the one it picks until the provider process exits.">
            <MarketingPillars
              ariaLabel="How xcb picks an account"
              columns={3}
              pillars={[
                { label: "Only accounts that can work now", summary: "Signed in, idle, not at a known usage limit, and on a model xcb has recently seen from that provider." },
                { label: "Ranked for the task", summary: "What is left is ranked by task type and by relative quality, cost, and speed." },
                { label: "One task per account", summary: "The provider runs in a sandbox with xcb’s file tools, and xcb holds the account until the provider process exits." },
              ]}
            />
            <a className="xcb-text-link" href="/docs/how-routing-works">How routing works →</a>
          </MarketingSection>

          <MarketingSection id="readiness" heading="What works today" headingId="readiness-title">
            <dl className="xcb-status-list">
              {readiness.map((row) => (
                <div key={row.name}>
                  <dt>{row.mark === null ? null : <ProviderMark mark={row.mark} label={row.name} size={20} />}{row.name}</dt>
                  <dd>{row.detail}</dd>
                </div>
              ))}
            </dl>
            <a className="xcb-text-link" href="/docs/providers">Supported builds and setup →</a>
          </MarketingSection>

          <MarketingSection id="install" heading="Install xcb" headingId="install-title" summary={publishedRelease === null ? "No release is published yet; build from source with Git and Rust 1.97.1." : "On macOS with Apple silicon or Linux x86_64, one command installs the latest release. Then connect Claude and open your thread."}>
            <CodeBlock code={publishedRelease === null ? "git clone https://github.com/hraness/xcb.git && cd xcb\n./scripts/install-native.sh" : `${installCommand}\nxcb setup claude\nxcb`} copyLabel="Copy commands" />
            <p className="xcb-install-links"><a className="xcb-text-link" href="/install">Install guide, including a prompt for your agent →</a></p>
          </MarketingSection>

          <MarketingQuestionList heading="Questions" headingId="questions-title" id="questions" questions={questions.map(({ question, answer }) => ({ question, answer: <p>{answer}</p> }))} />
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh" />
    </div>
  );
}
