import type { Metadata } from "next";
import { MarketingPage, MarketingSection, ProductHero } from "@hraness/design-kit/react/server";
import { AskAiAboutThis } from "@hraness/ui";
import { SiteHeader } from "../../site-header";
import { socialImages } from "../../social";

const title = "xcb vs OpenRouter: your subscriptions or a per-token API";
const description = "OpenRouter bills per token for API calls to hundreds of models. xcb sends each coding task to a Claude, Codex, or Devin plan you already pay for.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/compare/openrouter" },
  openGraph: { title, description, siteName: "xcb", type: "article", url: "/compare/openrouter", images: socialImages },
  twitter: { card: "summary_large_image", title, description, images: socialImages },
};

export default function CompareOpenRouter() {
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
            heading="xcb vs OpenRouter"
            headingId="compare-openrouter-title"
            summary="OpenRouter is a hosted API that sends each request from your code to one of hundreds of models and bills you per token. xcb runs on your machine and sends each coding task to one of the Claude, Codex, or Devin accounts you already pay for. Pick OpenRouter to call many models from an app; pick xcb to spread coding work across subscriptions you already have."
            actions={[{ href: "/docs/getting-started", label: "Try the source preview" }, { href: "/compare", label: "All comparisons" }]}
            boundary="Native xcb is a source preview for macOS and Linux."
          />

          <MarketingSection
            id="what"
            heading="What each product routes"
            headingId="what-title"
            summary="Both call themselves routers. OpenRouter chooses a model and provider for each API request; xcb chooses one of your accounts for each coding task."
          >
            <div className="xcb-comparison-fit">
              <div>
                <h3>OpenRouter: one API key for hundreds of models</h3>
                <p>Your code posts to OpenRouter’s OpenAI-compatible endpoint with one API key, and OpenRouter decides which provider serves the model you named, or picks a model for you with <code>openrouter/auto</code>. Its June 2026 routing guide describes 400+ models across 70+ providers, provider ordering by price, fallback chains across models and providers, and <code>:nitro</code> and <code>:floor</code> variants for throughput and price.</p>
                <p>Billing is pay-as-you-go per token in credits at the provider’s list price, with rate-limited <code>:free</code> models for trying the service. A BYOK path can send requests under your own provider keys instead.</p>
                <div className="xcb-comparison-sources"><a href="https://openrouter.ai">OpenRouter ↗</a><a href="https://openrouter.ai/pricing">Pricing ↗</a><a href="https://openrouter.ai/blog/insights/model-routing">Routing guide ↗</a></div>
              </div>
              <div>
                <h3>xcb: one of your accounts for each task</h3>
                <p>You sign in to your Claude, Codex, or Devin accounts on your own machine. For each task, xcb picks an account that is signed in, idle, and outside any known quota window, on a model it has recently seen in that provider’s catalog, and holds that account until the provider process exits.</p>
                <p>The provider’s own runtime runs the task under your subscription, so xcb has no token billing and no model catalog of its own, and it never proxies or rewrites API traffic. An unknown model name cannot activate a provider. Another agent can hand xcb a task with <code>xcb --json route</code>, and applications can embed the TypeScript SDK.</p>
                <div className="xcb-comparison-sources"><a href="/docs/providers">Supported accounts &amp; models</a><a href="/docs/route">Route tasks</a></div>
              </div>
            </div>
          </MarketingSection>

          <MarketingSection
            id="table"
            heading="Side by side"
            headingId="table-title"
          >
            <p className="xcb-compare-reviewed">Updated <time dateTime="2026-09-26">September 26, 2026</time>. OpenRouter details come from its site, pricing page, and routing guide on that date, and its catalog, routing options, and prices change. xcb details come from its own documentation.</p>
            <p className="xcb-compare-scroll-hint" id="openrouter-scroll-hint">On a small screen, scroll the table sideways to compare each approach.</p>
            <div className="xcb-comparison-scroll" role="region" aria-labelledby="openrouter-comparison-caption" aria-describedby="openrouter-scroll-hint" tabIndex={0}>
              <table className="xcb-comparison-table">
                <caption id="openrouter-comparison-caption">OpenRouter and xcb at a glance</caption>
                <thead>
                  <tr><th scope="col">Aspect</th><th scope="col">OpenRouter</th><th scope="col">xcb</th></tr>
                </thead>
                <tbody>
                  <tr>
                    <th scope="row">What it routes</th>
                    <td><p>One API request: a prompt in, a model’s answer out</p></td>
                    <td><p>One coding task; each route call runs one provider turn on one model</p></td>
                  </tr>
                  <tr>
                    <th scope="row">What you bring</th>
                    <td><p>An OpenRouter API key, or your own provider keys through its BYOK option</p></td>
                    <td><p>Claude, Codex, or Devin subscriptions you already pay for, signed in locally</p></td>
                  </tr>
                  <tr>
                    <th scope="row">Where it runs</th>
                    <td><p>OpenRouter’s hosted service; requests leave your machine for its endpoint</p></td>
                    <td><p>Your Mac or Linux machine; the provider’s own runtime makes the model calls</p></td>
                  </tr>
                  <tr>
                    <th scope="row">How you pay</th>
                    <td><p>Per token from a prepaid credit balance, at the provider’s list price; some <code>:free</code> models with daily rate limits</p></td>
                    <td><p>xcb charges nothing; usage draws on each subscription’s own allowance</p></td>
                  </tr>
                  <tr>
                    <th scope="row">Choosing the model</th>
                    <td><p>The <code>model</code> field on each request, or the <code>openrouter/auto</code> router</p></td>
                    <td><p>xcb picks from models recently seen on accounts that can take the task now, ranked by task type and by relative quality, cost, and latency</p></td>
                  </tr>
                  <tr>
                    <th scope="row">Choosing the provider</th>
                    <td><p>The <code>provider</code> object sets provider order, price ceiling, and region limits; price-ordered by default</p></td>
                    <td><p>The provider of the chosen account; a task stays on one account, so its requests are not spread across providers</p></td>
                  </tr>
                  <tr>
                    <th scope="row">When a call fails</th>
                    <td><p>Fallback chains retry on another model or provider automatically</p></td>
                    <td><p>The task stays on its account; when xcb cannot confirm how a run ended, it keeps the account held and does not retry</p></td>
                  </tr>
                </tbody>
              </table>
            </div>
            <p className="xcb-compare-note">OpenRouter also serves image, video, and speech models, which this table leaves out.</p>
          </MarketingSection>

          <MarketingSection
            id="fit"
            heading="Which one to pick"
            headingId="fit-title"
            layout="split"
          >
            <div className="xcb-comparison-fit">
              <div><h3>Pick OpenRouter to reach many models from your own code</h3><p>You want one key and one bill across many models, automatic fallbacks when a provider rate-limits or errors, or a model your subscriptions do not include. Any app that can call the OpenAI chat API can use it.</p></div>
              <div><h3>Pick xcb to spread coding tasks across plans you pay for</h3><p>You already pay for Claude, Codex, or Devin and want each coding task sent to an idle account and held there until its process exits, with no per-token charge. Your account credentials stay out of your project folder.</p></div>
              <div><h3>Compare cost on your own workload</h3><p>Per-token credits and a subscription’s usage allowance measure different things, so which one costs less depends on how you work.</p></div>
              <a className="xcb-compare-guide-link" href="/compare">Back to all comparisons →</a>
            </div>
          </MarketingSection>
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh/compare/openrouter" />
    </div>
  );
}
