/**
 * The film's words, from the site's launch facts and messaging, so the film
 * says what the launch post says. Numbers in the proof act come from
 * site/app/launch/facts.ts through `factNumber`; nothing here types a figure
 * that the facts module does not hold.
 */
import { launchMessaging } from "../site/app/launch/beats.ts";
import { launchFacts } from "../site/app/launch/facts.ts";
import type { FilmCopy } from "./timeline.ts";

const WORDS: Readonly<Record<string, number>> = { one: 1, two: 2, three: 3, four: 4, five: 5, six: 6, seven: 7, eight: 8, nine: 9, ten: 10 };

/** The leading number of a fact value, written as a word or digits: "five minutes" is 5, "24 hours" is 24. */
export function factNumber(value: string): number {
  const head = value.trim().split(/[\s-]/u)[0]!.toLowerCase();
  const parsed = WORDS[head] ?? Number(head);
  if (!Number.isFinite(parsed)) throw new Error(`The fact ${JSON.stringify(value)} does not start with a number.`);
  return parsed;
}

const tagline = launchMessaging.tagline;

export const filmCopy: FilmCopy = {
  name: "Excalibur",
  promise: tagline,
  url: "xcb.sh",
  open: ["One plan hits its limit.", "The others sit unused."],
  steps: [
    {
      heading: "One thread",
      body: "Type a task once. xcb picks the project and the account.",
      focus: "thread",
      target: "thread/thread-0",
      highlight: "thread/thread-2",
    },
    {
      heading: "Quota about to reset",
      body: "xcb favors unused quota that is about to reset.",
      focus: "accounts",
      target: "accounts/pick",
      highlight: "accounts/pick",
    },
    {
      heading: "Moves on at a limit",
      body: "At a reported usage limit, xcb can continue on an available account.",
      focus: "thread-limit",
      target: "thread-limit/thread-2",
      highlight: "thread-limit/thread-3",
    },
    {
      heading: "Runs in the background",
      body: "Close the terminal. xcb attention shows what needs you.",
      focus: "tasks",
      target: "tasks/attention",
      highlight: "tasks/attention",
    },
    {
      heading: "From any machine",
      body: "Send a task from your laptop to the desktop at home.",
      focus: "fleet",
      target: "fleet/dispatch",
      highlight: "fleet/dispatch",
    },
  ],
  proof: {
    caption: "Your own sign-ins. No API keys. Free and MIT licensed.",
    items: [
      { value: factNumber(launchFacts.providers.value), label: "providers: Claude, Codex, Devin" },
      { value: factNumber(launchFacts.meterMaxAge.value), suffix: " min", label: "longest a usage reading counts" },
    ],
  },
  limits: {
    heading: "What it doesn't do",
    body: "xcb doesn't raise any usage limit. It spends the quota you already have, in a better order.",
  },
  end: { line: `Free for Apple silicon Macs and Linux.` },
};
