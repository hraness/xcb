import type { Metadata } from "next";
import { MarketingPage, MarketingSection, ProductHero } from "@hraness/design-kit/react/server";
import { AskAiAboutThis } from "@hraness/ui";
import { SiteHeader } from "../site-header";
import { socialImages } from "../social";

const title = "Compare xcb with coding agents and developer tools";
const description = "See where xcb fits alongside Claude Code, Codex, OpenCode, and Devin. Compare product focus, workflows, and current limits.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/compare" },
  openGraph: { title, description, siteName: "xcb", type: "website", url: "/compare", images: socialImages },
  twitter: { card: "summary_large_image", title, description, images: socialImages },
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
            boundary="xcb picks one of your accounts and a model, runs the task, and keeps that account locked until the provider process has exited. Your providers’ access rules and usage limits still apply."
          />

          <MarketingSection
            id="approaches"
            heading="Which tool fits which job"
            headingId="approaches-title"
            summary="Start with the workflow you want. These products overlap, and you may use more than one."
          >
            <p className="xcb-compare-reviewed">Updated <time dateTime="2026-09-23">September 23, 2026</time>. Product descriptions link to official documentation. This is a comparison of focus, not a performance ranking.</p>
            <p className="xcb-compare-scroll-hint" id="comparison-scroll-hint">On a small screen, scroll the table sideways to compare each approach.</p>
            <div className="xcb-comparison-scroll" role="region" aria-labelledby="comparison-caption" aria-describedby="comparison-scroll-hint" tabIndex={0}>
              <table className="xcb-comparison-table">
                <caption id="comparison-caption">Coding agents and local workspaces</caption>
                <thead>
                  <tr><th scope="col">Approach</th><th scope="col">What it gives you</th><th scope="col">When it fits</th></tr>
                </thead>
                <tbody>
                  <tr className="xcb-comparison-own-row">
                    <th scope="row"><span className="xcb-comparison-name">xcb</span><span className="xcb-comparison-kind">Agent subscription router</span></th>
                    <td><p>A local router that picks one of your coding-agent accounts for each task and runs it there. Another agent can call it with a JSON command, and apps can embed the TypeScript SDK. xcb’s own terminal workspace is built on the same router.</p><a href="/docs/route">Route tasks</a></td>
                    <td><p>You use more than one supported provider account and want each task sent to an idle one, with that account locked until the provider process exits. Native xcb is a source preview with specific platform and command limits.</p><a href="/docs/workspace">Workspace limits</a></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Request routers</span><span className="xcb-comparison-kind">Subscription proxies</span></th>
                    <td><p>Forward API requests from one client across providers and subscription logins. Claude Code Router, Claudish, subswitch, and similar proxies work at the HTTP layer.</p><div className="xcb-comparison-sources"><a href="https://github.com/musistudio/claude-code-router">Claude Code Router</a><a href="https://claudish.com">Claudish</a><a href="https://github.com/dean0x/subswitch">subswitch</a></div></td>
                    <td><p>You want one client’s API requests rerouted across providers. xcb hands a whole task to one of your accounts and holds that account until the turn ends; it does not proxy or rewrite API traffic.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Superset</span><span className="xcb-comparison-kind">Agent workspace</span></th>
                    <td><p>Bring Claude Code, Codex, OpenCode, and other coding agents into one workspace. Run tasks in parallel, isolate changes, and review the results together.</p><div className="xcb-comparison-sources"><a href="https://superset.sh">Superset</a><a href="https://github.com/superset-sh/superset">Source</a></div></td>
                    <td><p>You want parallel agent runs with isolated changes in one workspace. xcb focuses on sending each task to one of your accounts rather than on reviewing parallel runs together.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Conductor</span><span className="xcb-comparison-kind">Parallel agent workspaces</span></th>
                    <td><p>A Mac app that runs Claude Code, Codex, Cursor, and OpenCode in parallel. Each task gets its own workspace, branch, terminal, and diff, and optional cloud workspaces are available.</p><div className="xcb-comparison-sources"><a href="https://docs.conductor.build">Conductor docs</a></div></td>
                    <td><p>You want several agents working in parallel, each on its own branch, with a review and merge flow. xcb focuses on which of your accounts runs each task, and keeps sessions and account state on your machine.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Paseo</span><span className="xcb-comparison-kind">Self-hosted agent daemon</span></th>
                    <td><p>Run Claude Code, Codex, Copilot, OpenCode, and Pi agents on your own machine with your full development environment, then connect from a phone, desktop, or browser.</p><div className="xcb-comparison-sources"><a href="https://paseo.sh">Paseo</a></div></td>
                    <td><p>You want to reach agents on your machine from other devices. xcb runs on your machine, from your terminal or from another program, and remote access is not its current focus.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">OpenChamber</span><span className="xcb-comparison-kind">Agentic desktop environment</span></th>
                    <td><p>A desktop and web development environment built on the OpenCode agent, with its own windowed interface for agent work.</p><div className="xcb-comparison-sources"><a href="https://openchamber.dev">OpenChamber</a><a href="https://github.com/openchamber/openchamber">Source</a></div></td>
                    <td><p>You want a graphical agent environment on top of OpenCode. xcb stays in the terminal and wraps the provider runtimes you already use.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Happy Coder</span><span className="xcb-comparison-kind">Mobile agent control</span></th>
                    <td><p>Control Claude Code, Codex, and other coding agents running on your computers from iOS, Android, or the web.</p><div className="xcb-comparison-sources"><a href="https://happy.engineering">Happy</a><a href="https://github.com/slopus/happy-cli">Source</a></div></td>
                    <td><p>You want to steer existing agent sessions from a phone. xcb runs on your computer and has no mobile app.</p></td>
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
                    <td><p>You want an agent that tunes its own context and skills. xcb’s experimental harness is being rebuilt to tune routing rules instead, testing them on labeled examples, while its provider checks, account locking, and run records stay fixed.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Devin cloud</span><span className="xcb-comparison-kind">Managed task environments</span></th>
                    <td><p>Delegate work into development environments with a shell, browser, and editor. Documented workflows can compare parallel Devin sessions.</p><div className="xcb-comparison-sources"><a href="https://docs.devin.ai/enterprise/deployment/overview">Deployment docs</a><a href="https://docs.devin.ai/use-cases/gallery/batch-3-agents-best-solution">Parallel workflow</a></div></td>
                    <td><p>You want managed remote task environments. xcb keeps its terminal and session state on your machine. Its Devin support drives the Devin CLI on macOS: the supported builds pass xcb’s sandbox checks without signing in, but a coding session on a signed-in Devin account hasn’t been confirmed. xcb checks your account’s model list when a Devin turn starts.</p><a href="/docs/providers#devin">xcb’s Devin status</a></td>
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
            summary="xcb is for developers, and the agents they run, who want each coding task sent to one of their own accounts with a record of where it ran."
          >
            <div className="xcb-comparison-fit">
              <div><h3>Choose xcb to spread tasks across your accounts.</h3><p>Send a task with the JSON command and you get back the account and model xcb chose, a session you can resume, and how the run ended. The account stays locked until the provider process has exited.</p></div>
              <div><h3>Keep provider tools for their full capabilities.</h3><p>The tested Claude and Codex setups passed coding workflows on macOS ARM64. xcb’s command runner currently uses offline Linux with prepared public dependencies and read-only Git inspection. Native macOS commands, Git commits, and pushes are outside that runner.</p></div>
              <div><h3>Use orchestration tools for a task graph.</h3><p>xcb’s current evidence covers account concurrency and controlled continuation, not a general fleet of agents planning and merging parallel work. It selects observed, supported models; an unknown model name cannot activate a provider.</p></div>
              <div><h3>Distinguish local state from local inference.</h3><p>xcb keeps its sessions and account state on your machine. Model requests still go to the selected provider. Subscription allowances remain separate, and unknown usage stays unknown.</p></div>
              <a className="xcb-compare-guide-link" href="/docs/getting-started">Read the setup guide →</a>
            </div>
          </MarketingSection>
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh/compare" />
    </div>
  );
}
