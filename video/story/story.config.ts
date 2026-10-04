/**
 * Excalibur's launch film: two coding plans with separate logins and limits,
 * an agent that only sees one, the reveal, one task routed by quota, moving a
 * task at a usage limit, background tasks, the trust boundary, and the
 * agent-install end card. Facts come from site/app/launch/facts.ts and the
 * portfolio messaging snapshot.
 */
import { join } from "node:path";

import { launchFacts } from "../../site/app/launch/facts.ts";
import { productMessaging } from "../../site/app/messaging.ts";
import { defineStory } from "./story.ts";
import palette from "./palette.json" with { type: "json" };

const repo = join(import.meta.dir, "../..");

export default () => defineStory({
  id: "xcb",
  brand: {
    wordmark: productMessaging.names.name,
    mark: join(repo, "site/public/marks/xcb.svg"),
    markAspect: 666 / 652,
    // Read with site-palette.ts from https://xcb.sh in dark mode; see palette.json.
    palette: { values: palette.palette },
    designKit: join(repo, "site/node_modules/@hraness/design-kit"),
  },
  acts: [
    {
      kind: "scatter", headline: "Two coding plans mean two logins and two usage limits.", accents: ["two", "logins"],
      cards: [
        { app: "Claude", glyph: "C", color: "#e0a07a", lines: ["Its own login", `Limits over ${launchFacts.claudeWindows.value} windows`] },
        { app: "Codex", glyph: "Cx", color: "#8fb0ff", lines: ["Another login", "Another reset time"] },
      ],
      ghosts: ["Terminal window", "Usage page", "Second terminal", "Copied prompt", "Reset timer"],
    },
    { kind: "chat", headline: "Unused quota is gone at the reset.", accents: ["gone"], exchanges: [
      { you: "Run this refactor on whichever plan has quota left.", agent: "I can only see the account I'm signed in to." },
    ] },
    { kind: "reveal", tagline: productMessaging.tagline },
    { kind: "chat", headline: "Type a task once.", accents: ["once."], label: "xcb", exchanges: [
      {
        you: "Refactor the billing module.",
        agent: "Started on the account with unused quota closest to its reset.",
        card: { kicker: "How it chose", title: "Usage read from every account", body: `Each reading is no more than ${launchFacts.meterMaxAge.value} old.` },
      },
    ] },
    {
      kind: "cards", headline: "It keeps the work moving across your plans.", accents: ["moving"],
      items: [
        { tag: "Usage limit", title: "Moves the task to another account", body: "With its original instructions, when a provider says an account hit its limit." },
        { tag: "Background", title: "Every task runs in the background", body: "xcb tasks lists them across all your projects." },
        { tag: "Your machines", title: "Send a task to the computer at home", body: "Task content is end-to-end encrypted on a relay you run." },
      ],
    },
    {
      kind: "cards", headline: "It runs under your own sign-in.", accents: ["your", "own"],
      items: [
        { tag: "Sign-in", title: "Claude Code or Codex, under your own account" },
        { tag: "Sandbox", title: "Each task runs in an OS sandbox" },
        { tag: "No API key", title: "It never falls back to an API key" },
      ],
    },
  ],
  end: {
    lead: "Ask your agent:", prompt: "Install xcb from xcb.sh",
    terms: "Free and MIT licensed · Apple silicon Macs and Linux", url: "xcb.sh",
  },
  formats: ["wide", "square"],
});
