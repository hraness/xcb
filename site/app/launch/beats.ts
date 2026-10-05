import {
  assertLaunchKit,
  buildSocialKit,
  resolveLaunchBeats,
  type LaunchBeat,
  type LaunchKitOptions,
  type LaunchMessaging,
  type LaunchRelease,
  type SocialKit,
} from "@hraness/design-kit/launch";
import { portfolioProducts } from "@hraness/design-kit/portfolio";

import { publishedRelease } from "../publication";
import { LAUNCH_STATUS, launchFacts } from "./facts";

/** The launch post's own URL. The last post of every thread ends with it. */
export const launchPostSlug = "one-agent-for-all-your-ai-plans";
export const launchPostUrl = `https://xcb.sh/blog/${launchPostSlug}`;

/**
 * The beats of "Introducing Excalibur". Each one is a short section of the
 * launch post and one post in the X, Bluesky, and Threads threads, so each
 * reads on its own. Numbers are {placeholders} filled from ./facts; the design
 * kit rejects a beat that types a digit.
 */
const authoredBeats: readonly LaunchBeat[] = [
  {
    id: "what",
    part: "what",
    headline: "Excalibur puts all your AI coding plans behind one agent",
    post: "Paying for Claude and Codex means two logins, two usage limits, and quota that expires unused. Excalibur (xcb) is a free terminal agent: type a task once, and it picks which of your accounts runs it.",
    visual: { kind: "mockup", id: "thread", state: { mode: "reset" } },
    alt: "Illustration of an xcb thread: you type one task, and xcb names the project and the account that runs it.",
  },
  {
    id: "quota",
    part: "does",
    headline: "It favors the quota that is about to reset",
    post: "Quota you don't use by the reset is gone. xcb reads each Claude and Codex account's usage, no more than {meterMaxAge} old, and favors unused quota that is about to reset.",
    visual: { kind: "mockup", id: "accounts", state: { mode: "reset" } },
    alt: "Illustration of the xcb accounts list: the account whose quota resets soonest gets the task.",
    facts: ["meterMaxAge"],
    detailHref: "/docs/how-routing-works",
  },
  {
    id: "limit",
    part: "does",
    headline: "Hit a limit mid-task and the task moves on",
    post: "When a provider says an account hit its usage limit partway through a task, xcb moves the task to another account or model that can take it, with its original instructions. You don't paste the prompt into a new window.",
    visual: { kind: "mockup", id: "accounts", state: { mode: "limit" } },
    alt: "Illustration of the xcb accounts list: one Claude account is at its limit and the task moves to the other.",
  },
  {
    id: "tasks",
    part: "does",
    headline: "Tasks keep running after you close the terminal",
    post: "Every task runs in the background. xcb tasks lists them across all your projects, and xcb attention collects the questions your agents are waiting on you to answer.",
    visual: { kind: "mockup", id: "tasks", state: { mode: "reset" } },
    alt: "Illustration of xcb tasks and xcb attention: one task running, one waiting on your answer, one done.",
    detailHref: "/docs/projects-and-tasks",
  },
  {
    id: "fleet",
    part: "does",
    headline: "Start work on your desktop from your laptop",
    post: "Link your machines with xcb link. From any of them, xcb fleet shows what each one is doing, and xcb dispatch sends a task to the one at home. Task content is end-to-end encrypted, on a relay you run yourself.",
    visual: { kind: "mockup", id: "fleet", state: {} },
    alt: "Illustration of xcb fleet on a laptop: three linked machines, and a task sent to the desktop at home.",
    detailHref: "https://github.com/hraness/xcb/blob/main/docs/remote-operations.md",
  },
  {
    id: "how",
    part: "how",
    headline: "Your own sign-ins, in each provider's own tool",
    post: "xcb runs Claude Code or Codex under your own sign-in, in an OS sandbox, and holds each account for one task at a time. It never falls back to an API key, and it never touches API traffic.",
    visual: { kind: "mockup", id: "run", state: { focus: "all" } },
    alt: "Illustration of how xcb runs a task: your sign-in, the provider's tool, a sandbox, xcb's file tools, your project.",
    detailHref: "/blog/introducing-excalibur",
  },
  {
    id: "who",
    part: "who",
    headline: "Made for people who run agents all day, and for agents",
    post: "xcb is for developers juggling more than one coding plan. It works for agents too: another program hands xcb a task as JSON and gets back which account ran it, how it ended, and the answer.",
    visual: { kind: "mockup", id: "route", state: {} },
    alt: "Illustration of xcb --json route: a task goes in as JSON and the account, outcome, and answer come back.",
    detailHref: "/docs/route",
  },
  {
    id: "vision",
    part: "vision",
    headline: "Put more of your subscription quota to work",
    post: "The idea behind Excalibur: use your coding plans together, from whichever machine you're at, so more work reaches accounts with quota to spare.",
    visual: { kind: "mockup", id: "thread", state: { mode: "limit" } },
    alt: "Illustration of an xcb thread: a task stops at one account's limit, continues on another, and finishes.",
  },
  {
    id: "limits",
    part: "limits",
    headline: "What xcb doesn't do",
    post: "xcb adds no quota or model access. Providers use xcb's file tools and registered host tools, without provider-native shells or unrelated plugins. Codex needs an Apple silicon Mac.",
    visual: { kind: "mockup", id: "run", state: { focus: "limits" } },
    alt: "Illustration of an xcb task, highlighting the tools that read and edit project files.",
  },
  {
    id: "status",
    part: "status",
    headline: "Free, open source, and on Apple silicon Macs and Linux",
    post: "Ask your agent to install xcb from xcb.sh, or install it yourself on a Mac with Apple silicon or on Linux, connect a Claude account with xcb setup claude, and open your thread with xcb. It is free and MIT licensed.",
    socialPost: "xcb is free and MIT licensed. {status}. Install it on a Mac with Apple silicon or on Linux, connect a Claude account with xcb setup claude, and open your thread with xcb.",
    visual: { kind: "mockup", id: "install", state: {} },
    alt: "Illustration of installing xcb: the one-line installer, xcb setup claude, and xcb.",
    facts: ["status"],
    detailHref: "/install",
  },
];

/** The xcb record in the portfolio registry. The kit's Product Hunt tagline and description come from it. */
const registry = portfolioProducts.xcb.messaging;

export const launchMessaging: LaunchMessaging = {
  names: { name: "Excalibur (xcb)" },
  tagline: registry.tagline,
  meta: registry.meta,
};

export const launchRelease: LaunchRelease = {
  status: LAUNCH_STATUS,
  tags: ["Developer Tools", "Artificial Intelligence", "Open Source"],
};

/** Tools the social posts never name; the comparison pages weigh them. */
export const forbiddenNames = [
  "Cursor",
  "herdr",
  "Herdr",
  "claude-swap",
  "Claude Code Router",
  "OpenRouter",
  "Pi",
  "Conductor",
] as const;

export const launchKitOptions: LaunchKitOptions = {
  status: LAUNCH_STATUS,
  publicInstall: publishedRelease !== null,
  tagline: launchMessaging.tagline,
  canonicalUrl: launchPostUrl,
  forbiddenNames,
};

export const launchBeats: readonly LaunchBeat[] = resolveLaunchBeats(authoredBeats, launchFacts);

export const socialKit: SocialKit = buildSocialKit(launchBeats, launchMessaging, launchRelease, launchPostUrl);

assertLaunchKit(launchBeats, socialKit, launchKitOptions);
