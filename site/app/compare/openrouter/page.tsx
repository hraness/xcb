import type { Metadata } from "next";
import { MarketingPage, MarketingSection, ProductHero } from "@hraness/design-kit/react/server";
import { AskAiAboutThis } from "@hraness/ui";
import { SiteHeader } from "../../site-header";
import { socialImages } from "../../social";

const title = "xcb compared with OpenRouter";
const description = "OpenRouter is one metered API across hundreds of models. xcb routes whole coding tasks across the Claude, Codex, and Devin subscriptions you already pay for.";

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
            heading="One routes API calls. One routes tasks."
            headingId="compare-openrouter-title"
            summary="OpenRouter gives your code one metered API in front of hundreds of models. xcb sends each coding task to one of the Claude, Codex, or Devin accounts you already pay for."
            actions={[{ href: "/docs/getting-started", label: "Try the source preview" }, { href: "/compare", label: "All comparisons" }]}
            boundary="Native xcb is a source preview for macOS and Linux. OpenRouter’s catalog, routing options, and prices change; this page records what its site, pricing page, and routing guide said on September 26, 2026."
          />

          <MarketingSection
            id="what"
            heading="What each one is"
            headingId="what-title"
            summary="Both products describe themselves as routers. They sit at different layers and answer different questions."
          >
            <div className="xcb-comparison-fit">
              <div>
                <h3>OpenRouter: one metered API across models</h3>
                <p>Your code posts to OpenRouter’s OpenAI-compatible endpoint with one API key, and OpenRouter decides which provider serves the model you named, or picks a model for you with <code>openrouter/auto</code>. Its June 2026 routing guide describes 400+ models across 70+ providers, provider ordering by price, fallback chains across models and providers, and <code>:nitro</code> and <code>:floor</code> variants for throughput and price.</p>
                <p>Billing is pay-as-you-go per token in credits at the provider’s list price, with rate-limited <code>:free</code> models for trying the service. A BYOK path can send requests under your own provider keys instead.</p>
                <div className="xcb-comparison-sources"><a href="https://openrouter.ai">OpenRouter ↗</a><a href="https://openrouter.ai/pricing">Pricing ↗</a><a href="https://openrouter.ai/blog/insights/model-routing">Routing guide ↗</a></div>
              </div>
              <div>
                <h3>xcb: your own subscriptions, one task at a time</h3>
                <p>xcb routes coding tasks, not API calls. You sign your Claude, Codex, or Devin accounts in on your own machine, and for each task xcb picks an account that is signed in, idle, and outside any known quota window, on a model it has recently seen in that provider’s catalog. It holds that account until the provider process exits.</p>
                <p>There is no token billing and no model catalog to browse: the provider’s own runtime runs the task under your subscription, and an unknown model name cannot activate a provider. Another agent can hand xcb a task with <code>xcb --json route</code>, and applications can embed the TypeScript SDK.</p>
                <div className="xcb-comparison-sources"><a href="/docs/providers">Supported accounts &amp; models</a><a href="/docs/route">Route tasks</a></div>
              </div>
            </div>
          </MarketingSection>

          <MarketingSection
            id="table"
            heading="Side by side"
            headingId="table-title"
            summary="One meters API calls for your code. The other multiplexes the subscriptions on your machine."
          >
            <p className="xcb-compare-reviewed">Updated <time dateTime="2026-09-26">September 26, 2026</time>. Sources: OpenRouter’s site, pricing page, and routing guide, and xcb’s own documentation. This is a comparison of layer and billing model, not a performance ranking.</p>
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
                    <td><p>xcb charges nothing. Usage draws on each subscription’s own allowance</p></td>
                  </tr>
                  <tr>
                    <th scope="row">Choosing the model</th>
                    <td><p>The <code>model</code> field on each request, or the <code>openrouter/auto</code> router</p></td>
                    <td><p>xcb picks from models recently seen on eligible accounts, ranked by task type and by relative quality, cost, and latency</p></td>
                  </tr>
                  <tr>
                    <th scope="row">Choosing the provider</th>
                    <td><p>The <code>provider</code> object sets provider order, price ceiling, and region limits; price-ordered by default</p></td>
                    <td><p>Whichever provider the account belongs to; xcb holds one account per task, so there is no per-request provider spread</p></td>
                  </tr>
                  <tr>
                    <th scope="row">When a call fails</th>
                    <td><p>Fallback chains retry on another model or provider automatically</p></td>
                    <td><p>One account holds the task. When xcb cannot confirm how a run ended, it keeps the account held and does not retry</p></td>
                  </tr>
                </tbody>
              </table>
            </div>
            <p className="xcb-compare-note">OpenRouter also serves image, video, and speech models, which this coding-task comparison does not cover. The fit guidance is our interpretation of documented capabilities; it does not claim that either product lacks billing controls, account features, or model coverage.</p>
          </MarketingSection>

          <MarketingSection
            id="fit"
            heading="Make the choice concrete."
            headingId="fit-title"
            layout="split"
            summary="They are not substitutes. OpenRouter meters model access for your code; xcb multiplexes the subscriptions on your machine."
          >
            <div className="xcb-comparison-fit">
              <div><h3>Choose OpenRouter when the model matters more than the account.</h3><p>You want one key and one bill across many models, automatic fallbacks when a provider rate-limits or errors, or a model your subscriptions do not include. Any app that can call the OpenAI chat API can use it.</p></div>
              <div><h3>Choose xcb when the account is the resource.</h3><p>You already pay for Claude, Codex, or Devin and want each coding task sent to an idle account and held there until its process exits, without metering tokens. Credentials stay out of your project folder.</p></div>
              <div><h3>They do not overlap.</h3><p>Nothing in OpenRouter routes whole tasks, and xcb never proxies or rewrites API traffic. OpenRouter decides which model answers an API call; xcb decides which of your accounts runs a task.</p></div>
              <div><h3>Watch what the bill measures.</h3><p>Per-token credit pricing and a subscription’s usage allowance are different meters, so neither product’s page can tell you which is cheaper for your workload.</p></div>
              <a className="xcb-compare-guide-link" href="/compare">Back to all comparisons →</a>
            </div>
          </MarketingSection>
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh/compare/openrouter" />
    </div>
  );
}
