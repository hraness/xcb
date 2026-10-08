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
 * at full size, or about 45 under a two-line headline, so each card gets that
 * page's point in a line of its own.
 * A path missing here falls back to the meta description, and the fit test
 * then fails until a card line is written.
 */
const cardCopy: Readonly<Record<string, { description: string; headline?: string }>> = {
  "/install": { description: "One command on macOS, Linux, or Windows, then connect an account and route a task." },
  "/reflexes": { description: "xcb answers for you once your replies certify it." },
  "/docs": { headline: "xcb documentation", description: "Install xcb, connect Claude or Codex, and send it work." },
  "/docs/getting-started": { description: "Install xcb, connect a Claude account, and send your first task." },
  "/docs/how-routing-works": { description: "How xcb picks an account and model for each task, and why it holds the account." },
  "/docs/security": { description: "What stays on your machine, what xcb sends out, and where credentials live." },
  "/docs/projects-and-tasks": { description: "Work across projects from one thread, and follow or resume long tasks." },
  "/docs/providers": { description: "Connect Claude or Codex accounts and pick an account or model." },
  "/docs/workspace": { description: "An offline Linux VM that runs your project's tests and builds on macOS ARM64." },
  "/docs/customization": { description: "Panes, continuation, context, deadlines, hooks, and the routing judge." },
  "/docs/reflexes": { description: "Inspect, teach, and roll back what xcb learns." },
  "/docs/upgrade-and-uninstall": { description: "Keep xcb current with verified releases, or remove it and its background jobs." },
  "/docs/troubleshooting": { description: "Fix provider builds, expired sign-ins, unfinished runs, and macOS folder access." },
  "/docs/route": { description: "Hand xcb one task as JSON. It picks the account and model and returns the result." },
  "/docs/sdk": { description: "Embed xcb's router in a TypeScript app and run tasks on the account you name." },
  "/docs/application-api": { description: "One model response per call for your app, with no tools or saved history." },
  "/docs/reference": { description: "Every xcb command, config setting, environment variable, and exit code." },
  "/compare": { description: "Where xcb fits beside coding agents, agent terminals, and routers." },
  "/compare/herdr": { description: "herdr keeps agents running in terminals you reattach to. xcb picks the account." },
  "/compare/pi": { description: "pi is a coding agent you extend in TypeScript. xcb routes tasks to your plans." },
  "/compare/claude-code": { description: "One plan: use it alone. Several plans: add xcb." },
  "/compare/conductor": { description: "Conductor gives each agent a worktree. xcb sends each task to one of your accounts." },
  "/compare/claude-code-router": { description: "It routes API requests. xcb routes whole tasks." },
  "/compare/opencode": { description: "OpenCode is an open agent for 75+ providers. xcb runs tasks on your own plans." },
  "/compare/openrouter": { description: "OpenRouter bills per token. xcb sends each task to a plan you already pay for." },
  "/blog": { description: "How xcb routes coding tasks across your Claude and Codex plans." },
  "/blog/one-agent-for-all-your-ai-plans": {
    headline: "Excalibur operates your AI subscriptions",
    description: "Log in with all your accounts, then tell your agent to use xcb.",
  },
  "/blog/introducing-excalibur": { description: "xcb sends each coding task to an idle Claude or Codex account of yours." },
  "/blog/how-xcb-uses-gobstopper": { description: "xcb trims stale tool output and keeps it locally." },
  "/blog/replayable-task-history": { description: "Replay a task offline and catch edited records." },
  "/blog/how-xcb-uses-algal": {
    headline: "How xcb uses ALGAL",
    description: "Continuation reflexes run as small ALGAL programs your own replies certify.",
  },
  "/blog/how-xcb-uses-wordcell": { description: "Workers search and cite your Wordcell notes." },
};

/**
 * One page's card. `path` gives the card its route-derived eyebrow
 * (Documentation, Comparison, Blog); `eyebrow` names one only where the route
 * does not. A section index whose headline already says its section
 * ("xcb documentation", "Excalibur (xcb) blog") carries no eyebrow.
 */
function page(path: string, headline: string, description: string, eyebrow?: string): [string, SocialImagePage] {
  const copy = cardCopy[path];
  return [path, { path, headline: copy?.headline ?? headline, description: copy?.description ?? description, ...(eyebrow === undefined ? {} : { eyebrow }) }];
}

/**
 * The copy on every route's share card, keyed by path. Each page card carries
 * that page's own headline and description, never the site tagline; the home
 * card (`/`) is the product card from the site declaration alone.
 */
export const socialCards: ReadonlyMap<string, SocialImagePage> = new Map<string, SocialImagePage>([
  ["/", {}],
  page("/install", installTitle, installDescription, "Get started"),
  page("/reflexes", reflexesHeadline, reflexesDescription, "Learned routing"),
  page("/docs", docsHeadline, docsDescription),
  ...docsTopics.map((topic) => page(`/docs/${topic.slug}`, topic.title, topic.description)),
  page("/compare", hubHeading, hubDescription),
  ...comparisons.map((entry) => page(comparisonPath(entry.slug), comparisonHeading(entry), entry.description)),
  page("/blog", blogTitle, blogDescription),
  ...blogPosts.map((entry) => page(blogPostPath(entry), entry.title, entry.dek, entry.eyebrow)),
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
