import type { Metadata } from "next";
import { MarketingPage, MarketingSection, ProductHero } from "@hraness/design-kit/react/server";
import { AskAiAboutThis } from "@hraness/ui";
import { SiteHeader } from "../site-header";

const title = "Compare xcb with coding agents and developer tools";
const description = "See where xcb fits alongside Claude Code, Codex, OpenCode, and Devin. Compare product focus, workflows, and current limits.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/compare" },
  openGraph: { title, description, siteName: "xcb", type: "website", url: "/compare" },
  twitter: { card: "summary_large_image", title, description },
};

export default function Compare() {
  return (
    <div data-hraness-marketing-preset="editorial" className="xcb-compare-page">
      <SiteHeader active="compare" />
      <main id="main" tabIndex={-1}>
        <MarketingPage>
          <ProductHero
            align="start"
            className="xcb-compare-hero"
            eyebrow="How xcb compares"
            name=""
            heading="Choose the layer you need."
            headingId="compare-title"
            summary="Some tools give you a coding agent. Others help you build an agent system. xcb routes work across the coding-agent accounts you already have."
            actions={[{ href: "/docs/getting-started", label: "Try the source preview" }, { href: "/docs/providers", label: "Check provider support" }]}
            boundary="xcb selects an eligible account/model route, runs one bounded turn, and proves custody at settlement. Provider access and usage limits still apply."
          />

          <MarketingSection
            id="approaches"
            heading="Different tools. Different jobs."
            headingId="approaches-title"
            summary="Start with the workflow you want. These products overlap, and you may use more than one."
          >
            <p className="xcb-compare-reviewed">Reviewed <time dateTime="2026-09-22">September 22, 2026</time>. Product descriptions link to official documentation. This is a comparison of focus, not a performance ranking.</p>
            <p className="xcb-compare-scroll-hint" id="comparison-scroll-hint">On a small screen, scroll the table sideways to compare each approach.</p>
            <div className="xcb-comparison-scroll" role="region" aria-labelledby="comparison-caption" aria-describedby="comparison-scroll-hint" tabIndex={0}>
              <table className="xcb-comparison-table">
                <caption id="comparison-caption">Coding agents and local workspaces</caption>
                <thead>
                  <tr><th scope="col">Approach</th><th scope="col">What it gives you</th><th scope="col">When it fits</th></tr>
                </thead>
                <tbody>
                  <tr className="xcb-comparison-own-row">
                    <th scope="row"><span className="xcb-comparison-name">xcb</span><span className="xcb-comparison-kind">Subscription router</span></th>
                    <td><p>A local router that selects an eligible route across your named coding-agent accounts and runs one bounded turn — callable as a JSON contract by another agent or embedded as a TypeScript SDK. A terminal workspace is the reference host.</p><a href="/docs/route">Route contract</a></td>
                    <td><p>You work across supported provider accounts and want work routed among them with proven custody. Native xcb is a source preview with specific platform and command limits.</p><a href="/docs/workspace">Workspace limits</a></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Request routers</span><span className="xcb-comparison-kind">Subscription proxies</span></th>
                    <td><p>Forward API requests from one client across providers and subscription logins — Claude Code Router, Claudish, subswitch, and similar proxies sit at the HTTP layer.</p><div className="xcb-comparison-sources"><a href="https://github.com/musistudio/claude-code-router">Claude Code Router</a><a href="https://claudish.com">Claudish</a><a href="https://github.com/dean0x/subswitch">subswitch</a></div></td>
                    <td><p>You want one client&apos;s requests rerouted across providers. xcb routes a whole task to an eligible account and owns its custody until the turn settles — it is not a request proxy and does not rewrite API traffic.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Superset</span><span className="xcb-comparison-kind">Agent workspace</span></th>
                    <td><p>Bring Claude Code, Codex, OpenCode, and other coding agents into one workspace. Run tasks in parallel, isolate changes, and review the results together.</p><div className="xcb-comparison-sources"><a href="https://superset.sh">Superset</a><a href="https://github.com/superset-sh/superset">Source</a></div></td>
                    <td><p>You want parallel agent runs with isolated changes in one workspace. xcb focuses on named accounts, local sessions, and usage rather than fleet-style parallel review.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Conductor</span><span className="xcb-comparison-kind">Cloud agent sandbox</span></th>
                    <td><p>Run a team of coding agents in isolated cloud sandboxes, each with its own copy of the repository.</p><div className="xcb-comparison-sources"><a href="https://docs.conductor.build">Conductor docs</a></div></td>
                    <td><p>You want managed cloud sandboxes for parallel agent work. xcb keeps sessions and account state on your machine instead of provisioning remote environments.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Paseo</span><span className="xcb-comparison-kind">Self-hosted agent daemon</span></th>
                    <td><p>Run Claude Code, Codex, Copilot, OpenCode, and Pi agents on your own machine with your full development environment, then connect from a phone, desktop, or browser.</p><div className="xcb-comparison-sources"><a href="https://paseo.sh">Paseo</a></div></td>
                    <td><p>You want to reach agents on your machine from other devices. xcb is the terminal workspace itself; remote access is not its current focus.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">OpenChamber</span><span className="xcb-comparison-kind">Agentic desktop environment</span></th>
                    <td><p>A desktop and web development environment built on the OpenCode agent, with its own windowed interface for agent work.</p><div className="xcb-comparison-sources"><a href="https://openchamber.dev">OpenChamber</a><a href="https://github.com/openchamber/openchamber">Source</a></div></td>
                    <td><p>You want a graphical agent environment on top of OpenCode. xcb stays in the terminal and wraps the provider runtimes you already use.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Happy Coder</span><span className="xcb-comparison-kind">Mobile agent control</span></th>
                    <td><p>Control Claude Code, Codex, and other coding agents running on your computers from iOS, Android, or the web.</p><div className="xcb-comparison-sources"><a href="https://happy.engineering">Happy</a><a href="https://github.com/slopus/happy-cli">Source</a></div></td>
                    <td><p>You want to steer existing agent sessions from a phone. xcb runs in a local terminal rather than a mobile companion.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Claude Code &amp; Codex</span><span className="xcb-comparison-kind">Provider coding tools</span></th>
                    <td><p>Work directly in a provider’s coding experience. Claude Code includes file editing, commands, integrations, and agent delegation. Codex offers its CLI, IDE extension, and cloud app surfaces for delegated tasks.</p><div className="xcb-comparison-sources"><a href="https://code.claude.com/docs/en/overview">Claude Code docs</a><a href="https://github.com/openai/codex">Codex source &amp; docs</a><a href="https://developers.openai.com/codex/app">Codex app</a></div></td>
                    <td><p>You want that provider’s full native experience. xcb wraps the exact supported provider runtimes with its own tools and terminal; it does not reproduce every provider feature.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">OpenCode</span><span className="xcb-comparison-kind">Multi-provider coding agent</span></th>
                    <td><p>Choose among model providers and configure primary agents and subagents with their own prompts, models, and tool access.</p><div className="xcb-comparison-sources"><a href="https://opencode.ai/docs/providers">Provider docs</a><a href="https://opencode.ai/docs/agents/">Agent docs</a></div></td>
                    <td><p>You want a configurable coding agent across model providers. xcb focuses on supported coding-agent runtimes and their accounts; its current provider coverage is narrower.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Prime Agent</span><span className="xcb-comparison-kind">Self-improving agent harness</span></th>
                    <td><p>An open-source coding agent built around a recursive LM control environment and a Continual Harness that refines prompts, memories, and subagent specs through evidence-backed updates.</p><div className="xcb-comparison-sources"><a href="https://github.com/PrimeIntellect-ai/prime-agent">Prime Agent</a><a href="https://primeintellect.ai">Prime Intellect</a></div></td>
                    <td><p>You want an agent that tunes its own context and skills. xcb&apos;s experimental harness evolves a different object — routing manifests evaluated on labeled cases — under admission, custody, and settlement contracts that stay fixed.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Devin cloud</span><span className="xcb-comparison-kind">Managed task environments</span></th>
                    <td><p>Delegate work into development environments with a shell, browser, and editor. Documented workflows can compare parallel Devin sessions.</p><div className="xcb-comparison-sources"><a href="https://docs.devin.ai/enterprise/deployment/overview">Deployment docs</a><a href="https://docs.devin.ai/use-cases/gallery/batch-3-agents-best-solution">Parallel workflow</a></div></td>
                    <td><p>You want managed remote task environments. xcb keeps its terminal and session state locally. Its Devin adapter uses the local ACP candidate, whose credential-free boundary proof is separate from authenticated coding acceptance. Account model availability is checked at launch.</p><a href="/docs/providers#devin">xcb’s Devin status</a></td>
                  </tr>
                </tbody>
              </table>
            </div>
            <p className="xcb-compare-note">This page focuses on user-facing coding tools. xcb is a local subscription router around those tools, not a general-purpose multi-agent task graph. The fit guidance is our interpretation of documented capabilities; it does not claim that other tools lack account controls, customization, local storage, or parallel work.</p>
          </MarketingSection>

          <MarketingSection
            id="fit"
            heading="Make the choice concrete."
            headingId="fit-title"
            layout="split"
            summary="xcb is for developers and agents who want one accountable route across their coding-agent subscriptions."
          >
            <div className="xcb-comparison-fit">
              <div><h3>Choose xcb for routed, accountable turns.</h3><p>Submit a task through the JSON contract or SDK and get back the selected route, a resumable session, and settled outcome facts — custody held until process exit is proven.</p></div>
              <div><h3>Keep provider tools for their full capabilities.</h3><p>The tested Claude and Codex setups passed coding workflows on macOS ARM64. xcb’s command runner currently uses offline Linux with prepared public dependencies and read-only Git inspection. Native macOS commands, Git commits, and pushes are outside that runner.</p></div>
              <div><h3>Use orchestration tools for a task graph.</h3><p>xcb’s current evidence covers account concurrency and controlled continuation, not a general fleet of agents planning and merging parallel work. It selects observed, supported models; an unknown model name cannot activate a provider.</p></div>
              <div><h3>Distinguish local state from local inference.</h3><p>xcb keeps its sessions and account state on your machine. Model requests still go to the selected provider. Subscription allowances remain separate, and unknown usage stays unknown.</p></div>
              <a className="xcb-compare-guide-link" href="/docs/getting-started">Read the setup guide ↗</a>
            </div>
          </MarketingSection>
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh/compare" />
    </div>
  );
}
