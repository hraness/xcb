import type { Metadata } from "next";
import { MarketingPage, MarketingSection, ProductHero } from "@hraness/design-kit/react/server";
import { AskAiAboutThis } from "@hraness/ui";
import { SiteHeader } from "../site-header";

const title = "Routing that learns how you work · xcb";
const description = "xcb learns which model tier you want and notices when a worker stopped before the job was done or is waiting for your go-ahead. Every decision is a replayable program, and learned changes are promoted only after they win on labels they were never fitted on.";
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
            summary="Workers end turns early: “Waiting on CI; I’ll merge on green.” “Should I open the PR?” And you pick a bigger model for the hard tasks. xcb learns all of it from what you already do, and shows its evidence."
            actions={[{ href: "/docs/reflexes", label: "Read how it works" }, { href: "/docs/getting-started", label: "Try the source preview" }]}
            boundary="Learning is local, stores numeric features rather than text, and never widens what a route or a continuation is allowed to do."
          />

          <MarketingSection
            id="what-it-learns"
            heading="Small decisions, made many times a day."
            headingId="what-it-learns-title"
            summary="Each is a reflex: a small ALGAL program over deterministic features and learned weights. The program has no effects and makes no model calls, so every decision is a receipt you can replay."
          >
            <div className="xcb-readiness">
              <div><h3>Which tier this task deserves</h3><p>The route reflex reads the shape of your request (an imperative opening, resume language, how many different actions it asks for) and, if configured, a judge&apos;s assessment. It chooses frontier or standard among routes that are already eligible. Large prompts always get the highest tier.</p></div>
              <div><h3>Whether the worker actually finished</h3><p>The settle reflex categorizes how each turn ended: done, stopped short, waiting for your go-ahead, asked a question, blocked, interrupted. Its defaults are fitted on 2,428 real follow-ups, where how much work a turn did was the strongest signal: “continue” followed 15% of turns with no tool calls and 45% of turns with 40 or more.</p></div>
              <div><h3>What you do next is the label</h3><p>Reply “continue” and xcb learns the turn stopped short; “yes, go ahead” teaches it the turn was asking; moving on means it was done. Ask for “opus” and the route learns you wanted more. When xcb continues for you, the continuation labels itself: real work confirms it, cancelling it counts against it.</p></div>
            </div>
          </MarketingSection>

          <MarketingSection
            id="gets-better"
            heading="Gets better as you use it. Only when it can prove it."
            headingId="gets-better-title"
            summary="Every 16 labels, xcb fits a challenger anchored on the shipped defaults. It has to beat the active generation on the next 48 labels, which neither has seen."
          >
            <div className="xcb-pane-example">
              <div className="xcb-evolve-loop" aria-label="The learning loop">
                <div className="xcb-evolve-step"><strong>observe</strong><span>each decision, as features</span></div>
                <span className="xcb-evolve-arrow" aria-hidden="true">→</span>
                <div className="xcb-evolve-step"><strong>label</strong><span>from your next move</span></div>
                <span className="xcb-evolve-arrow" aria-hidden="true">→</span>
                <div className="xcb-evolve-step"><strong>trial</strong><span>win on unseen labels</span></div>
              </div>
              <p>A challenger is promoted only if, on held-out labels that arrived after it was fitted, it lowers log loss without losing accuracy or ranking quality. Each generation records its parent and the trial that promoted it. <code>xcb reflex rollback settle 0</code> returns to the defaults.</p>
              <p>Replaying real operator history from a weak starting point, forward trials reached an AUC of 0.77, against 0.75 for a fixed one-in-five holdout and 0.58 for not learning at all, and none of your labels is withheld from learning forever. From a good starting point, trials mostly leave it alone.</p>
              <a className="xcb-text-link" href={`${reference}#measured-on-operator-history`}>Full measurements ↗</a>
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
              <div><h3>Bootstrap from history</h3><p><code>xcb reflex import</code> replays your own labeled history in order, reports how the reflex would have done, and adopts only heads that won a trial. It keeps derived features, not text.</p></div>
              <div><h3>Turn it up gradually</h3><p>Each reflex is off, observing, or active, and answering a go-ahead request has its own switch. Both ship observing. The deterministic safety gates, a risk veto for deletion, deployment, spending and credentials, and a configured judge&apos;s veto still apply.</p></div>
              <a className="xcb-compare-guide-link" href="/docs/reflexes">Read the reflex guide ↗</a>
            </div>
          </MarketingSection>
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh/reflexes" />
    </div>
  );
}
