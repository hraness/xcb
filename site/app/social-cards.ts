import { socialImageAlt, type SocialImagePage } from "@hraness/web-discovery/social-image/card";
import { blogDescription, blogPosts, blogPostPath, blogTitle } from "./blog/posts";
import { comparisonHeading, comparisonPath, comparisons } from "./compare/comparisons";
import { hubDescription, hubHeading } from "./compare/hub";
import { docsDescription, docsHeadline, docsTopics } from "./docs/topics";
import { installDescription, installTitle } from "./install/copy";
import { reflexesDescription, reflexesHeadline } from "./reflexes/copy";
import { socialSite } from "./social";

/**
 * Share-card copy that differs from the page's own meta description, keyed by
 * path. A meta description runs 110 to 160 characters; a card holds about 85
 * at full size, so each card gets that page's point in one line of its own.
 * A path missing here falls back to the meta description, and the fit test
 * then fails until a card line is written.
 */
const cardCopy: Readonly<Record<string, { description: string; headline?: string }>> = {
  "/install": { description: "One command on macOS, Linux, or Windows, then connect an account and route a task." },
  "/reflexes": { description: "xcb answers “continue” for you only after your own replies certify it." },
  "/docs": { description: "Install xcb, connect Claude, Codex, or Devin, and send it work." },
  "/docs/getting-started": { description: "Install xcb, connect a Claude account, and send your first task." },
  "/docs/how-routing-works": { description: "How xcb picks an account and model for each task, and why it holds the account." },
  "/docs/security": { description: "What stays on your machine, what xcb sends out, and where credentials live." },
  "/docs/projects-and-tasks": { description: "Work across projects from one thread, and follow or resume long tasks." },
  "/docs/providers": { description: "Connect Claude, Codex, or Devin accounts and pick an account or model." },
  "/docs/workspace": { description: "An offline Linux VM that runs your project's tests and builds on macOS ARM64." },
  "/docs/customization": { description: "Panes, continuation, context, deadlines, hooks, and the routing judge." },
  "/docs/reflexes": { description: "How xcb learns the model tier you want, and how to inspect and roll it back." },
  "/docs/upgrade-and-uninstall": { description: "Keep xcb current with verified releases, or remove it and its background jobs." },
  "/docs/troubleshooting": { description: "Fix provider builds, expired sign-ins, unfinished runs, and macOS folder access." },
  "/docs/route": { description: "Hand xcb one task as JSON. It picks the account and model and returns the result." },
  "/docs/sdk": { description: "Embed xcb's router in a TypeScript app and run tasks on the account you name." },
  "/docs/application-api": { description: "One model response per call for your app, with no tools or saved history." },
  "/docs/reference": { description: "Every xcb command, config setting, environment variable, and exit code." },
  "/compare": { description: "Where xcb fits beside coding agents, agent terminals, and routers." },
  "/compare/herdr": { description: "herdr keeps agents running in terminals you reattach to. xcb picks the account." },
  "/compare/pi": { description: "pi is a coding agent you extend in TypeScript. xcb routes tasks to your plans." },
  "/compare/claude-code": { description: "Use one alone when one plan covers you. Add xcb when you pay for several." },
  "/compare/conductor": { description: "Conductor gives each agent a worktree. xcb sends each task to one of your accounts." },
  "/compare/claude-code-router": { description: "Claude Code Router routes API requests. xcb hands whole tasks to your accounts." },
  "/compare/opencode": { description: "OpenCode is an open agent for 75+ providers. xcb runs tasks on your own plans." },
  "/compare/openrouter": { description: "OpenRouter bills per token. xcb sends each task to a plan you already pay for." },
  "/blog": { description: "How xcb routes coding tasks across your Claude, Codex, and Devin plans." },
  "/blog/one-agent-for-all-your-ai-plans": {
    headline: "Excalibur: one agent for all your AI plans",
    description: "Short posts on what xcb does when you pay for more than one AI coding plan.",
  },
  "/blog/introducing-excalibur": { description: "xcb sends each coding task to an idle Claude, Codex, or Devin account of yours." },
  "/blog/how-xcb-uses-gobstopper": { description: "xcb swaps old tool output for a short marker and keeps the original locally." },
  "/blog/replayable-task-history": { description: "xcb tasks verify replays a task offline and fails if a record was edited." },
  "/blog/how-xcb-uses-algal": {
    headline: "How xcb uses ALGAL",
    description: "Continuation reflexes run as small ALGAL programs your own replies certify.",
  },
  "/blog/how-xcb-uses-wordcell": { description: "Project workers search one Wordcell vault of your notes, citing each note." },
};

function page(path: string, eyebrow: string, headline: string, description: string): [string, SocialImagePage] {
  const copy = cardCopy[path];
  return [path, { eyebrow, headline: copy?.headline ?? headline, description: copy?.description ?? description }];
}

/**
 * The copy on every route's share card, keyed by path. Each page card carries
 * that page's own headline and description, never the site tagline; the home
 * card (`/`) is the product card from the site declaration alone.
 */
export const socialCards: ReadonlyMap<string, SocialImagePage> = new Map<string, SocialImagePage>([
  ["/", {}],
  page("/install", "Get started", installTitle, installDescription),
  page("/reflexes", "Learned routing", reflexesHeadline, reflexesDescription),
  page("/docs", "Docs", docsHeadline, docsDescription),
  ...docsTopics.map((topic) => page(`/docs/${topic.slug}`, "Docs", topic.title, topic.description)),
  page("/compare", "Compare", hubHeading, hubDescription),
  ...comparisons.map((entry) => page(comparisonPath(entry.slug), "Compare", comparisonHeading(entry), entry.description)),
  page("/blog", "Blog", blogTitle, blogDescription),
  ...blogPosts.map((entry) => page(blogPostPath(entry), entry.eyebrow, entry.title, entry.dek)),
]);

export function socialCard(path: string): SocialImagePage {
  const card = socialCards.get(path);
  if (card === undefined) throw new RangeError(`no share card is declared for ${path}`);
  return card;
}

/** The alt text for a route's share card, from the same copy the card draws. */
export function socialCardAlt(path: string): string {
  return socialImageAlt(socialSite, socialCard(path));
}
