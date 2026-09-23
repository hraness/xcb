import {
  MarketingCallToAction,
  MarketingInstallPanel,
  MarketingPage,
  MarketingQuestionList,
  MarketingRelated,
  MarketingSection,
  ProductHero,
} from "@hraness/design-kit/react/server";
import { AskAiAboutThis } from "@hraness/ui";
import { HeroField } from "./hero-field";
import { HeroGraphic } from "./hero-graphic";
import { publishedRelease } from "./publication";
import { CompatibilityArchive, NativeDownloads, ReleaseSummary } from "./release-state";
import { RoutePreview } from "./route-preview";
import { SiteHeader } from "./site-header";
import { WorkspacePreview } from "./workspace-preview";

const repository = "https://github.com/hraness/xcb";
const summary = "xcb routes coding tasks across the Claude, Codex, and Devin subscriptions you already pay for. Your agent calls a closed JSON contract, your app embeds the TypeScript SDK, or you drive the terminal — every turn lands on one eligible account, bounded, with custody proven at settlement.";
const questions = [
  { question: "What is xcb?", answer: "xcb, short for Excalibur, is an open-source subscription router for coding agents. It selects an eligible account/model route across your connected Claude, Codex, and Devin accounts, runs one bounded turn, and proves account custody when the work settles. A terminal workspace and an experimental managed harness are built on the same routing core. The native implementation is available as a source preview." },
  { question: "Can I use my existing AI subscriptions?", answer: "Yes — that is the point. Connect your own Claude, Codex, and Devin accounts and installed runtimes, and xcb routes work among them. It does not include model access, pool unrelated subscriptions, or remove provider usage limits; provider pricing and terms still apply." },
  { question: "Can another agent call it?", answer: "Yes. xcb --json route is a closed stdin/stdout contract: one task document in, one settled result out, including the selected route, a resumable session id, and outcome facts. A dryRun flag returns the selected route without reserving an account or launching a provider. Applications can embed the TypeScript SDK's createSubscriptionRouter instead." },
  { question: "How does it choose a route?", answer: "Candidates must be admitted runtimes on enabled, credentialed, idle accounts with observed fresh model entries, outside any known quota window. xcb then ranks survivors by task class and relative quality, cost, and latency Pareto tiers; an optional judge can only order routes that already passed. It never invents access and never substitutes an API route for a subscription route." },
  { question: "What happens when an account hits its limit?", answer: "A provider-reported quota window excludes that account's routes until it resets, so work goes to the next eligible subscription instead of failing against a blocked one. When nothing is eligible the call fails closed with a bounded unavailable result — xcb never silently substitutes an API key or an unqualified route." },
  { question: "What stays on my computer?", answer: "Account state, credentials custody, session history, and configuration stay local. Model requests still go to the provider the route selected. There is no required xcb cloud account, and local usage measurement does not automatically publish your data." },
  { question: "Is the managed harness ready?", answer: "No — it is experimental. The harness is being rebuilt as a self-evolving ALGAL harness, where routing manifests are proposed and evaluated on labeled cases, but the current build does not execute self-modifying orchestration policies. Admission, custody, and settlement contracts stay fixed while it evolves." },
  { question: "Which platforms does it run on?", answer: "Native release binaries are built for macOS ARM64 and Linux x86_64; other hosts build from source. Provider support is narrower than the platform list: Claude and Codex coding workflows have passed on macOS ARM64 with tested accounts, the Codex and Devin candidates currently require macOS, and the isolated command runner is set up on macOS ARM64." },
  { question: "Can I use it for daily work today?", answer: "The tested Claude and Codex setups passed real coding workflows on macOS ARM64. Native xcb is still a source preview: provider builds are restricted, command execution uses offline Linux, and the tested Devin account reached quota before a coding turn. Read the setup guide to decide whether those boundaries fit your projects." },
  { question: "What does it cost?", answer: "xcb is MIT licensed and free to build from source. Your provider subscriptions, model usage, and any services you choose are separate. There is no xcb subscription required to route through your own accounts." },
] as const;

const threeCommandInstall = `git clone https://github.com/hraness/xcb.git && cd xcb
rustup toolchain install 1.97.1 --profile minimal
./scripts/install-native.sh`;

export default function Home() {
  const structuredData = [
    { "@context": "https://schema.org", "@type": "SoftwareSourceCode", name: "xcb", description: summary, codeRepository: repository, programmingLanguage: ["Rust", "TypeScript"], license: "https://opensource.org/license/mit", url: "https://xcb.sh" },
    { "@context": "https://schema.org", "@type": "FAQPage", mainEntity: questions.map(({ question, answer }) => ({ "@type": "Question", name: question, acceptedAnswer: { "@type": "Answer", text: answer } })) },
  ];
  return (
    <div data-hraness-marketing-preset="editorial">
      <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: JSON.stringify(structuredData) }} />
      <SiteHeader active="home" />
      <main id="main" tabIndex={-1}>
        <MarketingPage>
          <div className="hraness-material-wall xcb-opening">
            <HeroField />
            <ProductHero className="xcb-marketing-hero" align="center" name=""
              heading="All your AI subscriptions. One router." headingId="hero-title" summary={summary}
              actions={[{ href: "/docs/getting-started", label: "Get started ↗" }, { href: "/docs/route", label: "Route contract" }]}
              boundary="Open source · uses the subscriptions you already have · source build"
              notice={<div className="xcb-hero-install"><p>Install from source in three commands. Needs Git and rustup; release binaries are built for macOS ARM64 and Linux x86_64.</p><pre className="install-command" tabIndex={0}><code>{threeCommandInstall}</code></pre><ReleaseSummary release={publishedRelease} /></div>}
              frame={<HeroGraphic />}
            />
          </div>
          <MarketingSection id="why" heading="Why xcb." headingId="why-title" summary="Most agent tools start from one seat. xcb starts from the subscriptions you already pay for — and from the agents that call it.">
            <div className="xcb-fit-grid">
              <div><h3>Multiplex your subscriptions</h3><p>One account&apos;s quota window shouldn&apos;t idle your day. Every credentialed account stays in the eligible set, and each turn lands on the best route that is actually free.</p></div>
              <div><h3>Made for other agents</h3><p>xcb is infrastructure, not just an app. A closed JSON contract lets any agent hand over a task and get a settled result back; the SDK embeds the same router in your application.</p></div>
              <div><h3>Evolves under fixed contracts</h3><p>Experimental: the managed harness proposes routing manifests, evaluates them on labeled cases, and promotes only strictly-better ones. What it may never change — admission, custody, settlement — stays fixed.</p></div>
            </div>
          </MarketingSection>
          <MarketingSection id="router" heading="Every turn. One accountable route." headingId="router-title" summary="A call is never just a prompt. The route that takes it is admitted, held, and settled on the record.">
            <div className="xcb-fit-grid">
              <div><h3>Custody that can be proven</h3><p>The selected account is held under an exclusive, generation-fenced lease while its provider runs. Credentials never enter the workspace, and custody stays held until process exit is independently proven.</p></div>
              <div><h3>Eligibility, not promises</h3><p>Only admitted runtimes, enabled credentialed accounts, and observed model entries are candidates. Known quota windows exclude a route; a public offer never stands in for live access.</p></div>
              <div><h3>One bounded turn</h3><p>Each call sets an explicit model, deadline, and output bound. Workspace tools are brokered, processes are joined, and an uncertain outcome is reported — never silently retried.</p></div>
            </div>
            <a className="xcb-text-link" href="/docs/route">Read the route contract ↗</a>
          </MarketingSection>
          <MarketingSection id="interfaces" heading="For agents, a contract. For apps, an SDK." headingId="interfaces-title" layout="split" summary="Another coding agent calls the contract. An application embeds the router.">
            <div className="xcb-interface-grid">
              <div><h3>For agents — <code>xcb --json route</code></h3><p>One closed JSON document on stdin, one bounded JSON result on stdout. The contract takes a task and eligibility constraints — never credentials, tools, hooks, or provider flags.</p><a className="xcb-text-link" href="/docs/route">Request and response schema ↗</a></div>
              <div><h3>For applications — the SDK</h3><p><code>createSubscriptionRouter</code> bundles the lease store and qualified task adapters into one call. The TypeScript compatibility package is a source build; hosts supply adapters and qualification evidence.</p><a className="xcb-text-link" href="/docs/route#sdk">Router entry point ↗</a></div>
            </div>
            <RoutePreview />
          </MarketingSection>
          <MarketingSection id="routing" heading="See the route before it runs." headingId="routing-title" layout="split-reverse" summary="No hidden brokerage. Every selection is a filter you can preview and a turn you can verify.">
            <div className="xcb-workflow-proof">
              <div className="xcb-proof-line"><span>Connected accounts</span><strong>eligible: admitted · credentialed · idle · outside known quota windows</strong></div>
              <div className="xcb-proof-connector" aria-hidden="true">↓</div>
              <div className="xcb-proof-line"><span>Observed model catalog</span><strong>Pareto tiers on relative quality · cost · latency</strong></div>
              <div className="xcb-proof-connector" aria-hidden="true">↓</div>
              <div className="xcb-proof-line"><span>Optional judge</span><strong>orders already-eligible routes only</strong></div>
              <div className="xcb-proof-connector" aria-hidden="true">↓</div>
              <div className="xcb-proof-line"><span>One routed turn</span><strong>joined processes · recorded outcome · resumable session</strong></div>
              <p>Preview selection without reserving an account: <code>xcb models route --task &quot;…&quot;</code> and <code>xcb models tiers --task &quot;…&quot;</code>. Verify managed task receipts with <code>xcb tasks verify</code>.</p>
              <a className="xcb-text-link" href="/docs/route">How selection works ↗</a>
            </div>
          </MarketingSection>
          <MarketingSection id="workspace" heading="Prefer to drive? There's a terminal." headingId="workspace-title" layout="split" summary="The xcb terminal puts the same router behind an interface you can drive yourself — conversations, sessions, panes, and usage in one place.">
            <WorkspacePreview />
          </MarketingSection>
          <MarketingSection id="harness" heading="A harness that improves itself." headingId="harness-title" summary="Experimental: the managed harness is being rebuilt as a self-evolving ALGAL harness. Routing manifests are proposed and evaluated on labeled cases, and a manifest is promoted only when it is strictly better — while admission, custody, and settlement contracts stay fixed and deterministic.">
            <div className="xcb-pane-example">
              <span className="xcb-badge" aria-label="Experimental feature">Experimental</span>
              <div className="xcb-evolve-loop" aria-label="The evolution loop">
                <div className="xcb-evolve-step"><strong>propose</strong><span>routing manifests</span></div>
                <span className="xcb-evolve-arrow" aria-hidden="true">→</span>
                <div className="xcb-evolve-step"><strong>evaluate</strong><span>labeled cases</span></div>
                <span className="xcb-evolve-arrow" aria-hidden="true">→</span>
                <div className="xcb-evolve-step"><strong>promote</strong><span>strictly better only</span></div>
              </div>
              <p>The current build does not execute self-modifying orchestration policies. The durable record format, exclusive account custody, and provider admission gates underneath it are the same ones the route contract uses — and they are fixed inputs to the evolution, not things it can rewrite.</p>
              <a className="xcb-text-link" href={`${repository}/blob/main/docs/managed-harness.md`}>Managed harness design ↗</a>
            </div>
          </MarketingSection>
          <MarketingSection id="readiness" heading="Honest about the edges." headingId="readiness-title" summary="Native xcb is a source preview. The current evidence covers specific builds and tested accounts, not every provider or machine.">
            <div className="xcb-readiness">
              <div><span className="xcb-status-dot" aria-hidden="true" /><h3>Claude &amp; Codex</h3><p>Installed coding workflows passed on macOS ARM64 with tested accounts and the exact supported builds: failing test, repair, passing test, and Git inspection.</p><a href="/docs/providers">Supported builds and setup ↗</a></div>
              <div><span className="xcb-status-dot xcb-status-caution" aria-hidden="true" /><h3>Devin</h3><p>The exact supported builds pass their sandbox-boundary checks without an account. A real coding session with your account still needs its own evidence; your account’s model list is checked at launch.</p><a href="/docs/providers#devin">Current Devin boundary ↗</a></div>
              <div><span className="xcb-status-dot xcb-status-neutral" aria-hidden="true" /><h3>Tests &amp; builds</h3><p>Offline Linux ARM64 commands, prepared public dependencies, and read-only Git inspection. No native macOS command execution.</p><a href="/docs/workspace">What the runner supports ↗</a></div>
            </div>
          </MarketingSection>
          <MarketingInstallPanel heading="Build the router." headingId="install-title" id="install">
            <p className="install-note">Start with the native source build — one binary serves the route contract, the terminal workspace, and the experimental harness. You’ll need Git, Rust 1.97.1, platform build tools, and a supported provider setup. Release binaries are built for macOS ARM64 and Linux x86_64; other hosts build from source.</p>
            <pre className="install-command" tabIndex={0}><code>{`git clone https://github.com/hraness/xcb.git
cd xcb
rustup toolchain install 1.97.1 --profile minimal
./scripts/install-native.sh
export PATH="$HOME/.local/bin:$PATH"
xcb --help
xcb update enable --policy notify   # macOS only: daily release check`}</code></pre>
            <a className="xcb-text-link" href="/docs/getting-started">Follow the complete setup guide ↗</a>
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
                  {
                    name: "Gobstopper",
                    href: "https://gobstopper.sh",
                    role: "Automatic context compaction for agent sessions",
                    relationship: "Gobstopper context management ships inside xcb as a default-on plugin, with bounded continuation and settled-boundary checks.",
                  },
                  {
                    name: "Ghostget",
                    href: "https://ghostget.com",
                    role: "A bounded bridge to provider data",
                    relationship: "Ghostget is the bounded web capability an xcb-routed agent can call: named, attested operations instead of a driven browser.",
                  },
                  {
                    name: "Aicharts",
                    href: "https://aicharts.io",
                    role: "AI model benchmarks and usage inspection",
                    relationship: "Aicharts benchmarks the models and subscription usage across providers; xcb's local usage measurement stays local and upload is unavailable.",
                  },
                ],
              },
              {
                heading: "The personal apps",
                headingId: "related-apps",
                items: [
                  {
                    name: "PeopleBlade",
                    href: "https://peopleblade.com",
                    role: "A private contact book for you and your agent",
                    relationship: "PeopleBlade's CLI is the kind of local, bounded surface an xcb-managed agent can drive against a real private domain.",
                  },
                  {
                    name: "Soulscrape",
                    href: "https://soulscrape.com",
                    role: "A dated, cited dossier on a person",
                    relationship: "Soulscrape turns authorized evidence into a cited working model, a bounded artifact an xcb task can produce and inspect.",
                  },
                  {
                    name: "Textbutler",
                    href: "https://textbutler.app",
                    role: "A personal message butler for Mac",
                    relationship: "Textbutler studies message history and drafts replies locally, the same bring-your-own-agent shape xcb's workspace organizes.",
                  },
                  {
                    name: "Wordcell",
                    href: "https://wordcell.io",
                    role: "A Markdown knowledge base for agents",
                    relationship: "Wordcell is the queryable vault behind an agent's notes and sources, local files an xcb session can search and cite.",
                  },
                ],
              },
            ]}
            heading="From the same workshop."
            headingId="related-title"
            label="Related"
            summary="Each Hraness product owns one private domain and gives your agent the same kind of access: local, bounded, and inspectable."
          />
          <MarketingCallToAction heading="Stop leaving subscriptions idle." headingId="cta-title" summary="One contract for your agents, one SDK for your applications — on the accounts you already pay for." actions={[{ href: "/docs/getting-started", label: "Get started ↗" }, { href: repository, label: "Explore the source" }]} footnote="xcb / Excalibur · Built by Hraness · MIT licensed" />
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh" />
    </div>
  );
}
