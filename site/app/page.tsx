import {
  MarketingCallToAction,
  MarketingInstallPanel,
  MarketingPage,
  MarketingQuestionList,
  MarketingSection,
  ProductHero,
} from "@hraness/design-kit/react/server";
import { AskAiAboutThis } from "@hraness/ui";
import { publishedRelease } from "./publication";
import { RoutePreview } from "./route-preview";
import { SiteHeader } from "./site-header";
import { WorkspacePreview } from "./workspace-preview";

const repository = "https://github.com/hraness/xcb";
const releaseVersion = publishedRelease?.version;
const summary = "xcb is an open-source subscription router for coding agents. Hand it a task — through the agent-facing JSON contract or the TypeScript SDK — and it picks an eligible account/model route across your own Claude, Codex, and Devin accounts, runs one bounded turn, and proves custody when the work settles.";
const questions = [
  { question: "What is xcb?", answer: "xcb, short for Excalibur, is an open-source subscription router for coding agents. It selects an eligible account/model route across your connected Claude, Codex, and Devin accounts, runs one bounded turn, and proves account custody when the work settles. A terminal workspace and an experimental managed harness are built on the same routing core. The native implementation is available as a source preview." },
  { question: "Can another agent call it?", answer: "Yes. xcb --json route is a closed stdin/stdout contract: one task document in, one settled result out, including the selected route, a resumable session id, and outcome facts. A dryRun flag returns the selected route without reserving an account or launching a provider. Applications can embed the TypeScript SDK's createSubscriptionRouter instead." },
  { question: "How does it choose a route?", answer: "Candidates must be admitted runtimes on enabled, credentialed, idle accounts with observed fresh model entries, outside any known quota window. xcb then ranks survivors by task class and relative quality, cost, and latency Pareto tiers; an optional judge can only order routes that already passed. It never invents access and never substitutes an API route for a subscription route." },
  { question: "Do I still need provider accounts?", answer: "Yes. Connect your own supported provider accounts and installed runtimes. xcb does not include model access, pool unrelated subscriptions, or remove provider usage limits. Provider pricing and terms still apply." },
  { question: "What stays on my computer?", answer: "Account state, credentials custody, session history, and configuration stay local. Model requests still go to the provider the route selected. There is no required xcb cloud account, and local usage measurement does not automatically publish your data." },
  { question: "Is the managed harness ready?", answer: "No — it is experimental. The harness is being rebuilt as a self-evolving ALGAL harness, where routing manifests are proposed and evaluated on labeled cases, but the current build does not execute self-modifying orchestration policies. Admission, custody, and settlement contracts stay fixed while it evolves." },
  { question: "Can I use it for daily work today?", answer: "The tested Claude and Codex setups passed real coding workflows on macOS ARM64. Native xcb is still a source preview: provider builds are restricted, command execution uses offline Linux, and the tested Devin account reached quota before a coding turn. Read the setup guide to decide whether those boundaries fit your projects." },
  { question: "What does it cost?", answer: "xcb is MIT licensed and free to build from source. Your provider subscriptions, model usage, and any services you choose are separate. There is no xcb subscription required to route through your own accounts." },
] as const;

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
            <ProductHero className="xcb-marketing-hero" align="center" name=""
              heading="Your subscriptions. Routed." headingId="hero-title" summary={summary}
              actions={[{ href: "/docs/getting-started", label: "Get started ↗" }, { href: "/docs/route", label: "Route contract" }]}
              boundary="Open source · your accounts, your quota · source build"
              frame={<RoutePreview />}
            />
          </div>
          <MarketingSection id="router" heading="Every routed turn carries the same evidence." headingId="router-title" summary="A call is never just a prompt. The route that takes it is admitted, held, and settled on the record.">
            <div className="xcb-fit-grid">
              <div><h3>Custody that can be proven</h3><p>The selected account is held under an exclusive, generation-fenced lease while its provider runs. Credentials never enter the workspace, and custody stays held until process exit is independently proven.</p></div>
              <div><h3>Eligibility, not promises</h3><p>Only admitted runtimes, enabled credentialed accounts, and observed model entries are candidates. Known quota windows exclude a route; a public offer never stands in for live access.</p></div>
              <div><h3>One bounded turn</h3><p>Each call sets an explicit model, deadline, and output bound. Workspace tools are brokered, processes are joined, and an uncertain outcome is reported — never silently retried.</p></div>
            </div>
            <a className="xcb-text-link" href="/docs/route">Read the route contract ↗</a>
          </MarketingSection>
          <MarketingSection id="interfaces" heading="Two ways in." headingId="interfaces-title" layout="split" summary="Another coding agent calls the contract. An application embeds the router.">
            <div className="xcb-interface-grid">
              <div><h3>For agents — <code>xcb --json route</code></h3><p>One closed JSON document on stdin, one bounded JSON result on stdout. The contract takes a task and eligibility constraints — never credentials, tools, hooks, or provider flags.</p><a className="xcb-text-link" href="/docs/route">Request and response schema ↗</a></div>
              <div><h3>For applications — the SDK</h3><p><code>createSubscriptionRouter</code> bundles the lease store and qualified task adapters into one call. The TypeScript compatibility package is a source build; hosts supply adapters and qualification evidence.</p><a className="xcb-text-link" href="/docs/route#sdk">Router entry point ↗</a></div>
            </div>
          </MarketingSection>
          <MarketingSection id="routing" heading="Routing in the open." headingId="routing-title" layout="split-reverse" summary="No hidden brokerage. Every selection is a filter you can preview and a turn you can verify.">
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
          <MarketingSection id="workspace" heading="One terminal is the reference host." headingId="workspace-title" layout="split" summary="The xcb terminal puts the same router behind an interface you can drive yourself — conversations, sessions, panes, and usage in one place.">
            <WorkspacePreview />
          </MarketingSection>
          <MarketingSection id="harness" heading="The managed harness — self-evolving, on fixed contracts." headingId="harness-title" summary="Experimental: the harness is being rebuilt as a self-evolving ALGAL harness. Routing manifests are proposed and evaluated on labeled cases, and a manifest is promoted only when it is strictly better — while admission, custody, and settlement contracts stay fixed and deterministic.">
            <div className="xcb-pane-example">
              <span className="xcb-badge" aria-label="Experimental feature">Experimental</span>
              <p>The current build does not execute self-modifying orchestration policies. The durable record format, exclusive account custody, and provider admission gates underneath it are the same ones the route contract uses.</p>
              <a className="xcb-text-link" href={`${repository}/blob/main/docs/managed-harness.md`}>Managed harness design ↗</a>
            </div>
          </MarketingSection>
          <MarketingSection id="readiness" heading="Useful today. Clear about the edges." headingId="readiness-title" summary="Native xcb is a source preview. The current evidence covers specific builds and tested accounts, not every provider or machine.">
            <div className="xcb-readiness">
              <div><span className="xcb-status-dot" aria-hidden="true" /><h3>Claude &amp; Codex</h3><p>Installed coding workflows passed on macOS ARM64: failing test, repair, passing test, and Git inspection.</p><a href="/docs/providers">Supported builds and setup ↗</a></div>
              <div><span className="xcb-status-dot xcb-status-caution" aria-hidden="true" /><h3>Devin</h3><p>Authentication and model discovery passed. The tested account hit provider quota before a coding turn.</p><a href="/docs/providers#devin">Current Devin boundary ↗</a></div>
              <div><span className="xcb-status-dot xcb-status-neutral" aria-hidden="true" /><h3>Tests &amp; builds</h3><p>Offline Linux ARM64 commands, prepared public dependencies, and read-only Git inspection. No native macOS command execution.</p><a href="/docs/workspace">What the runner supports ↗</a></div>
            </div>
          </MarketingSection>
          <MarketingInstallPanel heading="Build the router." headingId="install-title" id="install">
            <p className="install-note">Start with the native source build — one binary serves the route contract, the terminal workspace, and the experimental harness. You’ll need Git, Rust 1.97.1, platform build tools, and a supported provider setup.</p>
            <pre className="install-command" tabIndex={0}><code>{`git clone https://github.com/hraness/xcb.git
cd xcb
rustup toolchain install 1.97.1 --profile minimal
./scripts/install-native.sh
export PATH="$HOME/.local/bin:$PATH"
xcb update enable --policy notify
xcb --help`}</code></pre>
            <a className="xcb-text-link" href="/docs/getting-started">Follow the complete setup guide ↗</a>
            <details className="xcb-release-details"><summary>Package and release details</summary>
              <p>{releaseVersion === undefined ? "First xcb package release in preparation" : `Current verified compatibility release · v${releaseVersion}`}</p>
              {publishedRelease === null ? <p>No native xcb binary or <code>@hraness/xcb</code> npm package is published. The historical v0.3.0 archive is AgentMixer. <a href={`${repository}/releases`}>Check releases</a>.</p> : <p><a href={publishedRelease.archiveUrl}>Download the verified TypeScript compatibility archive</a> · <a href={publishedRelease.verificationRun}>Public release verification</a>. This is separate from the native Rust app.</p>}
            </details>
          </MarketingInstallPanel>
          <MarketingQuestionList heading="Before you begin." headingId="questions-title" id="questions" questions={questions.map(({ question, answer }) => ({ question, answer: <p>{answer}</p> }))} />
          <MarketingCallToAction heading="Your accounts. Routed." headingId="cta-title" summary="One contract for your agents, one SDK for your applications — on the subscriptions you already have." actions={[{ href: "/docs/getting-started", label: "Get started ↗" }, { href: repository, label: "Explore the source" }]} footnote="xcb / Excalibur · Built by Hraness · MIT licensed" />
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh" />
    </div>
  );
}
