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
import { CompatibilityArchive, NativeDownloads, ReleaseSummary } from "./release-state";
import { SiteHeader } from "./site-header";
import { WorkspacePreview } from "./workspace-preview";

const repository = "https://github.com/hraness/xcb";
const summary = "One terminal for your Claude Code, Codex, and Devin accounts. Prompts become durable tasks routed to an account that still has usage, project commands run in an isolated runner, and every session picks up where you left off.";
const questions = [
  { question: "What is xcb?", answer: "xcb, short for Excalibur, is an open-source terminal for developers who work with more than one coding agent. It brings your Claude Code, Codex, and Devin accounts, model choices, local sessions, and usage into one interface. The native Rust app is available as a source preview." },
  { question: "Is it a multi-agent orchestrator?", answer: "Its focus is the workspace around your coding agents: accounts, models, sessions, tools, and controlled continuation. It is not a hosted fleet of parallel agents or a visual workflow builder. Use a dedicated orchestrator when coordinating a task graph is your main need." },
  { question: "Do I still need provider accounts?", answer: "Yes. Connect your own supported provider accounts and installed runtimes. xcb does not include model access, pool unrelated subscriptions, or remove provider usage limits. Provider pricing and terms still apply." },
  { question: "What stays on my computer?", answer: "xcb keeps its account state, session history, configuration, and panes locally. Model requests still go to the provider you choose. There is no required xcb cloud account, and local usage measurement does not automatically publish your data." },
  { question: "Which platforms does it run on?", answer: "Native release binaries are built for macOS ARM64 and Linux x86_64; other hosts build from source. Provider support is narrower than the platform list: Claude and Codex coding workflows have passed on macOS ARM64 with tested accounts, the Codex and Devin candidates currently require macOS, and the isolated command runner is set up on macOS ARM64." },
  { question: "Can I use it for daily work today?", answer: "The tested Claude and Codex setups passed real coding workflows on macOS ARM64. Native xcb is still a source preview: provider builds are restricted and command execution uses offline Linux. Devin's exact builds pass their sandbox-boundary checks without an account; a real coding session with your account still needs its own evidence, and your account's model list is checked at launch. Read the setup guide to decide whether those boundaries fit your projects." },
  { question: "What does it cost?", answer: "xcb is MIT licensed and free to build from source. Your provider subscriptions, model usage, and any services you choose are separate. There is no xcb subscription required to use the local workspace." },
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
            <ProductHero className="xcb-marketing-hero" align="center" name=""
              heading="One terminal. Your coding agents." headingId="hero-title" summary={summary}
              actions={[{ href: "/docs/getting-started", label: "Get started ↗" }, { href: "/compare", label: "See how it compares" }]}
              boundary="Open source · runs on your machine · for developers who work with more than one coding agent"
              notice={<div className="xcb-hero-install"><p>Install from source in three commands. Needs Git and rustup; release binaries are built for macOS ARM64 and Linux x86_64.</p><pre className="install-command" tabIndex={0}><code>{threeCommandInstall}</code></pre><ReleaseSummary release={publishedRelease} /></div>}
              frame={<WorkspacePreview />}
            />
          </div>
          <MarketingSection id="workspace" heading="Less switching. More making." headingId="workspace-title" layout="split" summary="One place to choose an account, open a project, and get back to the work.">
            <div className="xcb-story-list">
              <div><span className="xcb-story-number">01</span><div><h3>Your accounts, together.</h3><p>Connect your Claude, Codex, and Devin accounts and pick the model you want. Credentials stay outside your projects, and the workflow stays the same across supported providers.</p></div></div>
              <div><span className="xcb-story-number">02</span><div><h3>Tasks that outlive the terminal.</h3><p>A prompt becomes a durable managed task. Closing the terminal detaches without cancelling it; open another terminal to check on the same work, answer a task that is waiting on you, or resume a saved session.</p></div></div>
              <div><span className="xcb-story-number">03</span><div><h3>Routing that reads the meter.</h3><p>Managed tasks go to a connected account that is signed in, idle, and has usage left, ranked by fit, remaining usage, and cost. Exhausted Claude accounts wait for their reported reset. Unknown usage stays unknown; nothing is invented.</p></div></div>
              <a className="xcb-text-link" href="/docs/providers">Accounts, models, and provider support ↗</a>
            </div>
          </MarketingSection>
          <MarketingSection id="workflow" heading="From a failing test to a change you can review." headingId="workflow-title" layout="split-reverse" summary="Read, edit, and test with workspace tools. Keep the result close enough to inspect.">
            <div className="xcb-workflow-proof">
              <div className="xcb-proof-line"><span>Project files</span><strong>Read &amp; edit</strong></div>
              <div className="xcb-proof-connector" aria-hidden="true">↓</div>
              <div className="xcb-proof-line"><span>Isolated Linux runner</span><strong>Test &amp; build</strong></div>
              <div className="xcb-proof-connector" aria-hidden="true">↓</div>
              <div className="xcb-proof-line"><span>Filtered Git view</span><strong>Inspect the diff</strong></div>
              <p>Commands run offline in a separate Linux VM with prepared public dependencies, so a test run cannot reach your credentials or the network. Native macOS tools and Git writes stay outside this runner.</p>
              <a className="xcb-text-link" href="/docs/workspace">Set up workspace commands ↗</a>
            </div>
          </MarketingSection>
          <MarketingSection id="make-it-yours" heading="A terminal that feels like yours." headingId="customize-title" layout="split" summary="Choose a focused pane, edit its layout, or generate a new one. Keep the working view while you try something different.">
            <div className="xcb-pane-example">
              <div className="xcb-pane-options" aria-label="Example pane commands"><code>/pane focus</code><code>/pane edit</code><code>/pane generate a compact view</code></div>
              <p>Your presentation can change without rebuilding the app. A layout cannot grant access to credentials or turn on executable hooks.</p>
              <a className="xcb-text-link" href="/docs/customization">Panes, sessions, and extensions ↗</a>
            </div>
          </MarketingSection>
          <MarketingSection id="compare" heading="Choose the right layer for your work." headingId="compare-title" summary="Coding assistants, cloud agents, and orchestration frameworks solve different problems. xcb focuses on your local coding workspace.">
            <div className="xcb-fit-grid">
              <div><h3>Work inside one provider</h3><p>A provider CLI gives you its native experience and newest provider-specific capabilities.</p></div>
              <div><h3>Delegate or build workflows</h3><p>Cloud agents take work into remote environments. Orchestration frameworks help you build coordinated agent applications.</p></div>
              <div><h3>Bring your workspace together</h3><p>xcb puts supported accounts, model choice, local history, and usage behind one terminal interface.</p></div>
            </div>
            <a className="xcb-text-link" href="/compare">Compare with Codex, Claude Code, OpenCode, and Devin ↗</a>
          </MarketingSection>
          <MarketingSection id="readiness" heading="Useful today. Clear about the edges." headingId="readiness-title" summary="Native xcb is a source preview. The current evidence covers specific builds and tested accounts, not every provider or machine.">
            <div className="xcb-readiness">
              <div><span className="xcb-status-dot" aria-hidden="true" /><h3>Claude &amp; Codex</h3><p>Installed coding workflows passed on macOS ARM64 with tested accounts and the exact supported builds: failing test, repair, passing test, and Git inspection.</p><a href="/docs/providers">Supported builds and setup ↗</a></div>
              <div><span className="xcb-status-dot xcb-status-caution" aria-hidden="true" /><h3>Devin</h3><p>The exact supported builds pass their sandbox-boundary checks without an account. A real coding session with your account still needs its own evidence; your account’s model list is checked at launch.</p><a href="/docs/providers#devin">Current Devin boundary ↗</a></div>
              <div><span className="xcb-status-dot xcb-status-neutral" aria-hidden="true" /><h3>Tests &amp; builds</h3><p>Offline Linux ARM64 commands, prepared public dependencies, and read-only Git inspection. No native macOS command execution.</p><a href="/docs/workspace">What the runner supports ↗</a></div>
            </div>
          </MarketingSection>
          <MarketingInstallPanel heading="Make room for better work." headingId="install-title" id="install">
            <p className="install-note">Start with the native source build. You’ll need Git, Rust 1.97.1, platform build tools, and a supported provider setup. Release binaries are built for macOS ARM64 and Linux x86_64; other hosts build from source.</p>
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
          <MarketingCallToAction heading="Your accounts. Your workflow." headingId="cta-title" summary="A local workspace for developers who work with more than one coding agent." actions={[{ href: "/docs/getting-started", label: "Get started ↗" }, { href: repository, label: "Explore the source" }]} footnote="xcb / Excalibur · Built by Hraness · MIT licensed" />
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh" />
    </div>
  );
}
