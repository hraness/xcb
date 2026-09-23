import type { Metadata } from "next";
import { MarketingPage, MarketingSection, ProductHero } from "@hraness/design-kit/react/server";
import { AskAiAboutThis } from "@hraness/ui";
import { SiteHeader } from "../site-header";

const title = "Routing that learns how you work · xcb";
const description = "xcb learns which model tier you want and notices when a worker stopped before the job was done. Every decision is a replayable program, and learned changes are promoted only on held-out evidence.";
const reference = "https://github.com/hraness/xcb/blob/main/docs/reflexes.md";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/reflexes" },
  openGraph: { title, description, siteName: "xcb", type: "website", url: "/reflexes" },
  twitter: { card: "summary_large_image", title, description },
};

export default function Reflexes() {
  return (
    <div data-hraness-marketing-preset="editorial" className="xcb-compare-page">
      <SiteHeader />
      <main id="main" tabIndex={-1}>
        <MarketingPage>
          <ProductHero
            align="start"
            className="xcb-compare-hero"
            eyebrow="Use case · learned routing"
            name=""
            heading="Stop typing “continue”."
            headingId="reflexes-title"
            summary="Workers end turns early: “Next, I’ll run the tests.” “Waiting on CI; I’ll merge on green.” And you pick a bigger model for the hard tasks. xcb learns both from what you already do, and shows its evidence."
            actions={[{ href: "/docs/reflexes", label: "Read how it works" }, { href: "/docs/getting-started", label: "Try the source preview" }]}
            boundary="Learning is local, stores numeric features rather than text, and never widens what a route or a continuation is allowed to do."
          />

          <MarketingSection
            id="what-it-learns"
            heading="Two small decisions, made many times a day."
            headingId="what-it-learns-title"
            summary="Each is a reflex: a small ALGAL program over deterministic features and learned weights. The program has no effects and makes no model calls, so every decision is a receipt you can replay."
          >
            <div className="xcb-readiness">
              <div><h3>Which tier this task deserves</h3><p>The route reflex reads the shape of your request (an imperative opening, resume language, how many different actions it asks for) and, if configured, a judge&apos;s assessment. It chooses frontier or standard among routes that are already eligible. Large prompts always get the highest tier.</p></div>
              <div><h3>Whether the worker actually finished</h3><p>The settle reflex categorizes how each turn ended: done, stopped short, asked a question, needs approval, blocked, interrupted. A promised next step, a worker waiting on an external event, or an open checklist count against “done”.</p></div>
              <div><h3>What you do next is the label</h3><p>Reply “continue” after a completed task and xcb learns the turn stopped short; with continuation on, it reopens that task in its session. Ask for “opus” after a task and the route learns you wanted more. Move on, and the turn counts as finished.</p></div>
            </div>
          </MarketingSection>

          <MarketingSection
            id="gets-better"
            heading="Gets better as you use it. Only when it can prove it."
            headingId="gets-better-title"
            summary="Every 16 labels, xcb fits a candidate anchored on the shipped defaults and compares it with the active generation on a holdout it never trains on."
          >
            <div className="xcb-pane-example">
              <div className="xcb-evolve-loop" aria-label="The learning loop">
                <div className="xcb-evolve-step"><strong>observe</strong><span>each decision, as features</span></div>
                <span className="xcb-evolve-arrow" aria-hidden="true">→</span>
                <div className="xcb-evolve-step"><strong>label</strong><span>from your next move</span></div>
                <span className="xcb-evolve-arrow" aria-hidden="true">→</span>
                <div className="xcb-evolve-step"><strong>promote</strong><span>held-out gain only</span></div>
              </div>
              <p>A candidate is promoted only with at least five held-out examples of each outcome, lower log loss, and no loss of accuracy or ranking quality. Each generation records its parent and the evidence that promoted it. <code>xcb reflex rollback route 0</code> returns to the defaults.</p>
              <p>On one operator&apos;s history of 320 first prompts, the keyword fallback had no signal for the tier they picked (holdout AUC 0.44). After one training pass, the learned head reached 0.65 on the same holdout without a judge call. The judged head, already fitted to that operator, was not promoted because the holdout showed no gain.</p>
              <a className="xcb-text-link" href={`${reference}#measured-on-one-operators-history`}>Full measurements ↗</a>
            </div>
          </MarketingSection>

          <MarketingSection
            id="malleable"
            heading="Change the logic, not just the weights."
            headingId="malleable-title"
            summary="Parameters are data and programs are replaceable, so the learned part and the decision logic are separate seams."
          >
            <div className="xcb-readiness">
              <div><h3>Replace a program</h3><p>Drop an organism at <code>reflexes/route.algal.json</code> in the state directory to add a gate or combine heads differently. xcb admits it only if it has no effects and no agent calls. Every observation records the digest of the program that made it.</p></div>
              <div><h3>Bootstrap from history</h3><p><code>xcb reflex import</code> reads your own labeled prompts and keeps only the derived features, so you can start from months of history instead of from zero.</p></div>
              <div><h3>Turn it up gradually</h3><p>Each reflex is off, observing, or active. Continuation ships observing: it categorizes and learns, but does not act until you turn it on. The deterministic safety gates and a configured judge&apos;s veto still apply.</p></div>
              <a className="xcb-compare-guide-link" href="/docs/reflexes">Read the reflex guide ↗</a>
            </div>
          </MarketingSection>
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh/reflexes" />
    </div>
  );
}
