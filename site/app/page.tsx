import {
  MarketingCallToAction,
  MarketingInstallPanel,
  MarketingPage,
  MarketingQuestionList,
  MarketingRelated,
  MarketingSection,
  ProductHero,
  ProviderMark,
} from "@hraness/design-kit/react/server";
import { product, type PortfolioProductId } from "@hraness/design-kit/portfolio";
import { AskAiAboutThis } from "@hraness/ui";
import { HeroField } from "./hero-field";
import { HeroGraphic } from "./hero-graphic";
import { publishedRelease } from "./publication";
import { CompatibilityArchive, NativeDownloads, ReleaseSummary } from "./release-state";
import { RoutePreview } from "./route-preview";
import { SiteHeader } from "./site-header";
import { WorkspacePreview } from "./workspace-preview";

const repository = "https://github.com/hraness/xcb";

/** A related-product card from the portfolio snapshot: mark, name, and one-line role. */
function related(id: PortfolioProductId, name: string) {
  const { canonicalUrl, mark, oneLiner } = product(id);
  return { href: canonicalUrl, mark, name, role: oneLiner };
}
const summary = "One terminal for your Claude, Codex, and Devin accounts. Each task runs on an account that is signed in, idle, and not at a known limit.";
const metaDescription = "xcb routes coding tasks across the Claude, Codex, and Devin subscriptions you already pay for, picking an account that is signed in and idle.";
const questions = [
  { question: "What is xcb?", answer: "xcb, short for Excalibur, is an open-source router for coding agents. It picks one of your connected Claude, Codex, or Devin accounts and a model, runs the task, and holds that account until the provider process has exited. A terminal workspace and an experimental managed harness are built on the same router. The native app is a source preview." },
  { question: "Can I use my existing AI subscriptions?", answer: "Yes. Connect your own Claude, Codex, and Devin accounts and installed CLIs, and xcb routes work among them. It does not include model access, pool unrelated subscriptions, or lift provider usage limits; provider pricing and terms still apply." },
  { question: "Can another agent call it?", answer: "Yes. xcb --json route reads one task as JSON on stdin and writes one JSON result on stdout: the route it chose, a session id you can resume, and how the run ended. With dryRun set, it returns the chosen route without reserving an account or starting a provider. Applications can embed the TypeScript SDK’s createSubscriptionRouter instead." },
  { question: "How does it choose a route?", answer: "First it filters. A candidate needs a supported provider build, an enabled, signed-in, idle account outside any known quota window, and a model recently seen in that provider’s catalog. Then it ranks what is left by task type and by relative quality, cost, and latency. An optional judge can inform that ranking but cannot add a route that failed the filter. xcb never invents access and never swaps a subscription route for an API route." },
  { question: "What happens when an account hits its limit?", answer: "When the provider has reported a quota window for an account, xcb skips that account until the reported reset, so work goes to the next account that qualifies. If none qualify, a route call returns an unavailable result and a managed task waits for the reset. xcb never falls back to an API key or an unsupported route." },
  { question: "What stays on my computer?", answer: "Account state, credentials, session history, and configuration stay on your computer. Model requests still go to the provider the route selected. If you turn on the optional judge, which is off by default, it sends limited task and response context to TypeSafe’s System One service. You don’t need an xcb cloud account, and local usage measurement never publishes your data automatically." },
  { question: "Is the managed harness ready?", answer: "No, it is experimental. It is being rebuilt on ALGAL to propose routing rules and test them on labeled examples, and the current build does not execute self-modifying orchestration policies. What it learns today are reflexes: two small programs that pick a model tier for a new task and categorize how a turn ended. They learn from your replies, adopt new parameters only after they beat the current ones on labels they were not fitted on, and can be rolled back. Provider checks, account locking, and run records stay fixed." },
  { question: "Which platforms does it run on?", answer: "The release pipeline builds binaries for macOS ARM64 and Linux x86_64; other hosts build from source. Provider support is narrower than the platform list: Claude and Codex coding workflows have passed on macOS ARM64 with the tested accounts, Codex and Devin support currently requires macOS, and the isolated command runner is set up on macOS ARM64." },
  { question: "Can I use it for daily work today?", answer: "The tested Claude and Codex setups passed real coding workflows on macOS ARM64. Native xcb is still a source preview: provider builds are restricted, command execution uses offline Linux, and the tested Devin account reached quota before a coding turn. Read the setup guide to decide whether those limits fit your projects." },
  { question: "What does it cost?", answer: "xcb is MIT licensed. Your provider subscriptions, model usage, and any services you choose are separate. There is no xcb subscription required to route through your own accounts." },
] as const;

const threeCommandInstall = `git clone https://github.com/hraness/xcb.git && cd xcb
rustup toolchain install 1.97.1 --profile minimal
./scripts/install-native.sh`;

export default function Home() {
  const structuredData = [
    { "@context": "https://schema.org", "@type": "SoftwareSourceCode", name: "xcb", description: metaDescription, codeRepository: repository, programmingLanguage: ["Rust", "TypeScript"], license: "https://opensource.org/license/mit", url: "https://xcb.sh" },
    { "@context": "https://schema.org", "@type": "FAQPage", mainEntity: questions.map(({ question, answer }) => ({ "@type": "Question", name: question, acceptedAnswer: { "@type": "Answer", text: answer } })) },
  ];
  return (
    <div data-hraness-marketing-preset="editorial">
      <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: JSON.stringify(structuredData) }} />
      <SiteHeader active="home" />
      <main id="main" tabIndex={-1}>
        <MarketingPage>
          <div className="hraness-material-wall xcb-opening">
            <ProductHero backdrop={<HeroField />} className="xcb-marketing-hero" align="start" name="xcb"
              eyebrow="Agent subscription router"
              heading="Keep coding when one subscription hits its limit." headingId="hero-title" summary={summary}
              actions={[{ href: "/docs/getting-started", label: "Install xcb" }, { href: repository, label: "View the source ↗" }]}
              boundary="Source preview · MIT licensed · macOS ARM64 and Linux x86_64"
              notice={<div className="xcb-hero-install"><p>{publishedRelease === null
                ? "Build from source. You need Git, rustup, and your platform’s build tools."
                : <>Build from source with Git, rustup, and your platform’s build tools, or <a href="/download">download a verified release</a>.</>}</p><pre className="install-command" tabIndex={0}><code>{threeCommandInstall}</code></pre><ReleaseSummary release={publishedRelease} /></div>}
              frame={<HeroGraphic />}
            />
          </div>
          <MarketingSection id="why" heading="Why xcb." headingId="why-title" summary="Use the coding subscriptions you already pay for, whether you start a task or another agent does.">
            <div className="xcb-fit-grid">
              <div><h3>Keep working when an account hits a known quota limit</h3><p>When an account reaches a known quota limit, xcb skips it until the provider’s reported reset and sends the next task to the best-ranked account and model that is free.</p></div>
              <div><h3>Made for other agents</h3><p>Another agent can hand xcb a task as one JSON request and get back the result, the route it took, and a session it can resume. Apps can use the TypeScript SDK instead, which holds the account the app names while its task runs.</p></div>
              <div><h3>Self-tuning, in development</h3><p>The managed harness is being rebuilt to propose routing rules, test them on labeled examples, and keep only rules that score strictly better. The design keeps provider checks, account locking, and run records outside what it can change.</p></div>
            </div>
          </MarketingSection>
          <MarketingSection id="router" heading="What happens when xcb routes a task" headingId="router-title" summary="xcb checks which accounts can take the task, holds the one it picks, and records how the run ended.">
            <div className="xcb-fit-grid">
              <div><h3>The account stays locked until the run ends</h3><p>While the provider runs, xcb holds the chosen account so no other task can start on it. Credentials never enter your project folder, and xcb releases the account only after it has independent proof that the provider process exited.</p></div>
              <div><h3>Only accounts that pass xcb’s checks</h3><p>A candidate needs a supported provider build, an enabled and signed-in account, and a model xcb has recently seen in that provider’s catalog. Accounts inside a known quota window are skipped. A provider’s advertised promotion never counts as proof that your account qualifies for it.</p></div>
              <div><h3>One turn per route call</h3><p>Each route call runs one provider turn on one model, with an optional deadline and a cap on the returned text. File access goes through xcb’s broker, xcb waits for the provider’s processes to exit, and when the outcome is uncertain it says so instead of retrying.</p></div>
            </div>
            <a className="xcb-text-link" href="/docs/route">How routing works →</a>
          </MarketingSection>
          <MarketingSection id="interfaces" heading="Call it from an agent or embed it in an app." headingId="interfaces-title" layout="split" summary="Coding agents use the JSON command. Applications can embed the TypeScript SDK.">
            <div className="xcb-interface-grid">
              <div><h3>For agents: <code>xcb --json route</code></h3><p>An agent writes one JSON request to stdin and reads one JSON result from stdout. The request carries the task and can pin a provider, account, model, or timeout. It cannot carry credentials, tools, hooks, system prompts, or provider flags.</p><a className="xcb-text-link" href="/docs/route">Request and response schema →</a></div>
              <div><h3>For applications: the SDK</h3><p><code>createSubscriptionRouter</code> wraps your account lease store and task adapters in one object. Your app names the account and model for each task, and the router holds that account while the task runs. Your app supplies each provider adapter, with proof that its exact provider build runs with the expected tools and file access.</p><a className="xcb-text-link" href="/docs/route#sdk">Router entry point →</a></div>
            </div>
            <RoutePreview />
          </MarketingSection>
          <MarketingSection id="routing" heading="See the route before it runs." headingId="routing-title" layout="split-reverse" summary="You can preview which account and model xcb would choose before it reserves anything, and check a managed task’s record after it runs.">
            <div className="xcb-workflow-proof">
              <div className="xcb-proof-line"><span>Connected accounts</span><strong>supported build · signed in · idle · outside known quota windows</strong></div>
              <div className="xcb-proof-connector" aria-hidden="true">↓</div>
              <div className="xcb-proof-line"><span>Observed model catalog</span><strong>ranked by relative quality · cost · latency</strong></div>
              <div className="xcb-proof-connector" aria-hidden="true">↓</div>
              <div className="xcb-proof-line"><span>Optional judge</span><strong>informs the ranking, never adds a route</strong></div>
              <div className="xcb-proof-connector" aria-hidden="true">↓</div>
              <div className="xcb-proof-line"><span>One routed turn</span><strong>processes exit before release · outcome recorded · session you can resume</strong></div>
              <p>Preview selection without reserving an account: <code>xcb models route --task &quot;…&quot;</code> and <code>xcb models tiers --task &quot;…&quot;</code>. Check that a managed task’s local record is consistent with <code>xcb tasks verify</code>.</p>
              <a className="xcb-text-link" href="/docs/route">How selection works →</a>
            </div>
          </MarketingSection>
          <MarketingSection id="workspace" heading="Prefer to drive? There's a terminal." headingId="workspace-title" layout="split" summary="The xcb terminal puts the same router behind an interface you drive yourself, with conversations, sessions, panes, and usage in one place.">
            <WorkspacePreview />
          </MarketingSection>
          <MarketingSection id="harness" heading="In development: a harness that tunes its own routing" headingId="harness-title" summary="The managed harness is being rebuilt on ALGAL so it can propose routing rules, test them on labeled examples, and keep a rule only when it scores strictly better. Provider checks, account locking, and run records stay fixed and deterministic.">
            <div className="xcb-pane-example">
              <span className="xcb-badge" aria-label="Experimental feature">Experimental</span>
              <div className="xcb-evolve-loop" aria-label="The evolution loop">
                <div className="xcb-evolve-step"><strong>propose</strong><span>routing rules</span></div>
                <span className="xcb-evolve-arrow" aria-hidden="true">→</span>
                <div className="xcb-evolve-step"><strong>evaluate</strong><span>labeled cases</span></div>
                <span className="xcb-evolve-arrow" aria-hidden="true">→</span>
                <div className="xcb-evolve-step"><strong>keep</strong><span>strictly better only</span></div>
              </div>
              <p>What ships today are <a href="/reflexes">reflexes</a>: they learn which model tier you want and notice when a worker stopped short, and they adopt new parameters only after they beat the current ones on labels they were never fitted on. Their programs never rewrite themselves, and the current build does not execute self-modifying orchestration policies. The harness uses the same run records, account locking, and provider checks as the route command, and it cannot rewrite them.</p>
              <a className="xcb-text-link" href={`${repository}/blob/main/docs/managed-harness.md`}>Managed harness design ↗</a>
            </div>
          </MarketingSection>
          <MarketingSection id="readiness" heading="What works today" headingId="readiness-title" summary="Native xcb is a source preview. These results come from specific builds and tested accounts, so your machine and accounts may differ.">
            <div className="xcb-readiness">
              <div><span className="xcb-status-dot" aria-hidden="true" /><h3><span className="xcb-provider-marks"><ProviderMark mark="claudecode" label="Claude Code" size={24} /><ProviderMark mark="codex" label="Codex" size={24} /></span>Claude &amp; Codex</h3><p>On macOS ARM64, with the tested accounts and the supported builds, each finished a real coding task: run a failing test, fix the code, pass the test, and inspect Git status.</p><a href="/docs/providers">Supported builds and setup →</a></div>
              <div><span className="xcb-status-dot xcb-status-caution" aria-hidden="true" /><h3><span className="xcb-provider-marks"><ProviderMark mark="devin" label="Devin" size={24} /></span>Devin</h3><p>The supported Devin builds pass xcb’s sandbox checks without signing in. A coding session on a signed-in Devin account hasn’t been confirmed yet. xcb checks your account’s model list each time a Devin turn starts.</p><a href="/docs/providers#devin">Current Devin status →</a></div>
              <div><span className="xcb-status-dot xcb-status-neutral" aria-hidden="true" /><h3>Tests &amp; builds</h3><p>Commands run offline in a Linux ARM64 VM, with public dependencies you prepare in advance. Git access is read-only, and native macOS commands can’t run.</p><a href="/docs/workspace">What the runner supports →</a></div>
            </div>
          </MarketingSection>
          <MarketingInstallPanel heading="Build the router." headingId="install-title" id="install">
            <p className="install-note">Start with the source build. One binary serves the route command, the terminal workspace, and the experimental harness. You need Git, Rust 1.97.1, your platform’s build tools, and a supported provider setup.</p>
            <pre className="install-command" tabIndex={0}><code>{`git clone https://github.com/hraness/xcb.git
cd xcb
rustup toolchain install 1.97.1 --profile minimal
./scripts/install-native.sh
export PATH="$HOME/.local/bin:$PATH"
xcb --help
xcb update enable --policy notify   # macOS only: daily release check`}</code></pre>
            <a className="xcb-text-link" href="/docs/getting-started">Follow the setup guide →</a>
            <details className="xcb-release-details"><summary>Release downloads and package details</summary>
              <NativeDownloads release={publishedRelease} />
              <CompatibilityArchive release={publishedRelease} />
            </details>
          </MarketingInstallPanel>
          <MarketingQuestionList heading="Before you begin." headingId="questions-title" id="questions" questions={questions.map(({ question, answer }) => ({ question, answer: <p>{answer}</p> }))} />
          <MarketingRelated
            groups={[
              {
                heading: "The agent platform",
                headingId: "related-tools",
                summary: "The layer your agent runs through: sessions, accounts, web reads, and the models behind them.",
                items: [
                  related("gobstopper", "Gobstopper"),
                  related("wrench", "Ghostget"),
                  related("aicharts", "AI Charts"),
                ],
              },
              {
                heading: "The personal apps",
                headingId: "related-apps",
                items: [
                  related("peopleblade", "PeopleBlade"),
                  related("soulscrape", "Soulscrape"),
                  related("message-like-me", "Textbutler"),
                  related("kb", "Wordcell"),
                ],
              },
            ]}
            heading="From the same workshop."
            headingId="related-title"
            label="Related"
            summary="Each Hraness product owns one private domain and gives your agent the same kind of access: local, limited to a stated boundary, and inspectable."
          />
          <MarketingCallToAction heading="Stop leaving subscriptions idle." headingId="cta-title" summary="Send work to the accounts you already pay for, from your agents with the JSON command or from your apps with the SDK." actions={[{ href: "/docs/getting-started", label: "Get started" }, { href: repository, label: "Explore the source" }]} footnote="xcb · Built by Hraness · MIT licensed" />
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh" />
    </div>
  );
}
