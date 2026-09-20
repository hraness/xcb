import type { Metadata } from "next";
import { MarketingPage, MarketingSection, ProductHero } from "@hraness/design-kit/react/server";
import { AskAiAboutThis } from "@hraness/ui";
import { SiteHeader } from "../site-header";

const title = "Compare xcb with coding agents and orchestration tools";
const description = "See where xcb fits alongside Claude Code, Codex, OpenCode, Devin cloud, and LangGraph. Compare product focus, workflows, and current limits.";

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
            summary="Some tools give you a coding agent. Others help you build an agent system. xcb focuses on the local workspace around your coding-agent accounts."
            actions={[{ href: "/docs/getting-started", label: "Try the source preview" }, { href: "/docs/providers", label: "Check provider support" }]}
            boundary="xcb brings accounts, models, sessions, and usage into one terminal. Provider access and usage limits still apply."
          />

          <MarketingSection
            id="approaches"
            heading="Different tools. Different jobs."
            headingId="approaches-title"
            summary="Start with the workflow you want. These products overlap, and you may use more than one."
          >
            <p className="xcb-compare-reviewed">Reviewed <time dateTime="2026-09-20">September 20, 2026</time>. Product descriptions link to official documentation. This is a comparison of focus, not a performance ranking.</p>
            <p className="xcb-compare-scroll-hint" id="comparison-scroll-hint">On a small screen, scroll the table sideways to compare each approach.</p>
            <div className="xcb-comparison-scroll" role="region" aria-labelledby="comparison-caption" aria-describedby="comparison-scroll-hint" tabIndex={0}>
              <table className="xcb-comparison-table">
                <caption id="comparison-caption">Coding agents, local workspaces, and orchestration frameworks</caption>
                <thead>
                  <tr><th scope="col">Approach</th><th scope="col">What it gives you</th><th scope="col">When it fits</th></tr>
                </thead>
                <tbody>
                  <tr className="xcb-comparison-own-row">
                    <th scope="row"><span className="xcb-comparison-name">xcb</span><span className="xcb-comparison-kind">Local account workspace</span></th>
                    <td><p>Named coding-agent accounts, model choice, local sessions, and usage in one terminal, with workspace tools and explicit runtime support.</p><a href="/docs/providers">Provider support</a></td>
                    <td><p>You work across supported provider accounts and want a common workflow. Native xcb is a source preview with specific platform and command limits.</p><a href="/docs/workspace">Workspace limits</a></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Claude Code &amp; Codex CLI</span><span className="xcb-comparison-kind">Provider coding tools</span></th>
                    <td><p>Work directly in a provider’s coding experience. Claude Code includes file editing, commands, integrations, and agent delegation. Codex CLI runs locally and supports ChatGPT sign-in.</p><div className="xcb-comparison-sources"><a href="https://code.claude.com/docs/en/overview">Claude Code docs</a><a href="https://github.com/openai/codex">Codex source &amp; docs</a></div></td>
                    <td><p>You want that provider’s full native experience. xcb wraps admitted runtimes with its own tools and terminal; it does not reproduce every provider feature.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">OpenCode</span><span className="xcb-comparison-kind">Multi-provider coding agent</span></th>
                    <td><p>Choose among model providers and configure primary agents and subagents with their own prompts, models, and tool access.</p><div className="xcb-comparison-sources"><a href="https://opencode.ai/docs/providers">Provider docs</a><a href="https://opencode.ai/docs/agents/">Agent docs</a></div></td>
                    <td><p>You want a configurable coding agent across model providers. xcb focuses on supported coding-agent runtimes and their accounts; its current provider coverage is narrower.</p></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">Devin cloud</span><span className="xcb-comparison-kind">Managed task environments</span></th>
                    <td><p>Delegate work into development environments with a shell, browser, and editor. Documented workflows can compare parallel Devin sessions.</p><div className="xcb-comparison-sources"><a href="https://docs.devin.ai/enterprise/deployment/overview">Deployment docs</a><a href="https://docs.devin.ai/use-cases/gallery/batch-3-agents-best-solution">Parallel workflow</a></div></td>
                    <td><p>You want managed remote task environments. xcb keeps its terminal and session state locally. Its Devin adapter uses the local ACP candidate, a separate path whose tested account hit quota before coding acceptance.</p><a href="/docs/providers#devin">xcb’s Devin status</a></td>
                  </tr>
                  <tr>
                    <th scope="row"><span className="xcb-comparison-name">LangGraph</span><span className="xcb-comparison-kind">Orchestration framework</span></th>
                    <td><p>Build custom stateful agent workflows with persistence, durable execution, and human review points.</p><a href="https://docs.langchain.com/oss/python/langgraph/overview">LangGraph overview</a></td>
                    <td><p>You need to design an application’s workflow graph. xcb provides a terminal workspace and bounded application integration around admitted provider accounts.</p><a href="/docs/application-api">Application integration</a></td>
                  </tr>
                </tbody>
              </table>
            </div>
            <p className="xcb-compare-note">The fit guidance is our interpretation of these documented capabilities. It does not claim that other tools lack account controls, customization, local storage, or parallel work.</p>
          </MarketingSection>

          <MarketingSection
            id="fit"
            heading="Make the choice concrete."
            headingId="fit-title"
            layout="split"
            summary="xcb is for developers who want to bring the administration around their coding agents into one place."
          >
            <div className="xcb-comparison-fit">
              <div><h3>Choose xcb for a shared local workflow.</h3><p>Select a named account, choose an observed model, reopen a saved session, and inspect usage without changing terminal interfaces.</p></div>
              <div><h3>Keep provider tools for their full capabilities.</h3><p>The tested Claude and Codex setups passed coding workflows on macOS ARM64. xcb’s command runner currently uses offline Linux with prepared public dependencies and read-only Git inspection. Native macOS commands, Git commits, and pushes are outside that runner.</p></div>
              <div><h3>Use orchestration tools for a task graph.</h3><p>xcb’s current evidence covers account concurrency and controlled continuation, not a general fleet of agents planning and merging parallel work. It selects observed, admitted models; an unknown model name cannot activate a provider.</p></div>
              <div><h3>Distinguish local state from local inference.</h3><p>xcb keeps its sessions and account state on your machine. Model requests still go to the selected provider. Subscription allowances remain separate, and unknown usage stays unknown.</p></div>
              <a className="xcb-compare-guide-link" href="/docs/getting-started">Read the setup guide ↗</a>
            </div>
          </MarketingSection>
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.dev/compare" />
    </div>
  );
}
