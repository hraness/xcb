import { blogPostPath, indexableBlogPosts } from "../blog/posts";
import { providerStatus, supportedBuilds } from "../docs/provider-status";
import { docsTopics } from "../docs/topics";
import { nativePlatforms, publicationMarkdown, publishedRelease } from "../publication";

// llms.txt is a machine-readable site summary. Its release claim comes from
// the same signed-off datum as every public page, so it cannot drift.
export const dynamic = "force-static";

const releasedPlatforms = nativePlatforms
  .filter(({ platform }) => publishedRelease?.native.some((asset) => asset.platform === platform))
  .map(({ label }) => label);
const releaseLine = publishedRelease === null
  ? "No native xcb binary or @hraness/xcb package archive is published yet; build from source."
  : releasedPlatforms.length === 0
    ? "The verified @hraness/xcb package archive is available; native binaries must be built from source."
    : `Verified release archives cover ${releasedPlatforms.join(", ")}; other hosts build from source.`;
const releaseDetails = publicationMarkdown(publishedRelease);
const docsLines = docsTopics.map((topic) => `- [${topic.title}](https://xcb.sh/docs/${topic.slug}): ${topic.description}`).join("\n");
// Only indexable posts are listed; quarantined posts stay out of machine-readable maps.
const blogLines = indexableBlogPosts.map((entry) => `- [${entry.title}](https://xcb.sh${blogPostPath(entry)}): ${entry.dek}`).join("\n");

const body = `# Excalibur (xcb)

> Excalibur (xcb) is a tool for operating AI subscriptions. Log in with all your Claude and Codex accounts, then tell your agent to use xcb.

xcb is a command-line router for developers who pay for more than one coding agent, and for the agents and apps that work for them. Each task runs on an account that is signed in, idle, and not at a known usage limit, on a model that fits the work, and xcb holds that account until the provider process exits. It is MIT licensed. ${releaseLine}

## Use it as a coding agent

Set up with \`xcb setup claude\` or \`xcb setup codex\`. \`xcb run -p "<task>"\` runs one task in the current project folder, picks an account and model, and prints the answer. \`xcb backlog add <project-folder> "<task>" --ready\` hands a task to xcb's background supervisor, so it keeps running after the command returns; \`xcb tasks\` and \`xcb attention\` show progress and questions waiting for you. A managed task that stops at a usage limit continues on another account or model. xcb has no interactive terminal; other agents and your own tools read its \`--json\` output instead.

## See your token use

\`xcb usage\` shows token use across your coding agents by day, agent, provider, and model, from aicharts' daily record on your computer; nothing is uploaded. The installer adds aicharts on macOS with Apple silicon and Linux x86_64. \`xcb usage connect\` gives routed Claude and Codex tasks aicharts' read-only usage tools.

## Use subscription capacity before it resets

When Claude or Codex reports fresh usage data, xcb favors unused capacity approaching a reset among routes that meet the task's quality requirements. It considers overlapping usage windows and respects explicit account and model choices. Missing or stale meters add no preference. [How routing works](https://xcb.sh/docs/how-routing-works) explains the selection.

## Continue conversations you already started

\`xcb sessions discover\` finds Claude and Codex conversations active in the last 24 hours by default. \`xcb sessions import --recent\` copies their user and assistant messages into xcb. \`xcb conversations\` lists the imports, and \`xcb backlog add <conversation-id> "<task>" --ready\` continues one with normal routing. The original files stay in place; importing starts no tasks and does not take control of running provider sessions. [Session import](https://xcb.sh/docs/projects-and-tasks#import-sessions) covers the commands and workspace selection.

## Build on it

- Agents and scripts: \`xcb --json route\` reads one JSON task on stdin (\`version\`, \`workspace\`, \`task\`, optional \`provider\`, \`account\`, \`model\`, \`timeoutMs\`, \`dryRun\`), picks the account and model, runs one turn, and prints one JSON result. It exits 0 only for a completed turn; failures carry a \`code\` such as \`unavailable\`, \`busy\`, \`needs_input\`, or \`custody_unproven\`.
- TypeScript apps: \`createSubscriptionRouter\` from the \`@hraness/xcb\` SDK runs a task on the account and model the app names and holds that account until the provider exits. It does not choose the account or model. Install it with \`npm install @hraness/xcb\` or download its release archive.
- Apps that need one tool-free model response: \`xcb --json generate\`, after the build, account, and model pass xcb's application checks.

## Providers and limits

- Claude: Claude Code ${supportedBuilds.claudeMinimum} or later within version 2. ${providerStatus.claude} On Linux, Claude runs after xcb's sandbox checks pass on that machine.
- Codex: Codex CLI ${supportedBuilds.codex.join(", ")} on macOS ARM64. ${providerStatus.codex}
- Providers work through xcb's file tools and the host MCP servers you register, in an OS sandbox, without their own shells or unrelated plugins. Model requests go from each provider's CLI to that provider.
- The command runner for tests and builds is an offline Linux VM on macOS ARM64. Git is read-only there, and native macOS builds can't run.
- Each account runs one provider turn at a time by default (\`max_runs_per_account\` raises it), and tasks in the same project folder take turns.
- The self-tuning managed harness is in development; the current build does not run self-modifying routing policies.

${releaseDetails === "" ? "" : `## Verified release\n\n${releaseDetails}\n\n`}## Documentation

${docsLines}

## Pages

- [Overview](https://xcb.sh/): what xcb does, how it routes, and how to install it.
- [Install](https://xcb.sh/install): the one-line installer, first steps, a prompt for your agent, and the source build.
- [Documentation](https://xcb.sh/docs): all guides, grouped for using xcb and building on it.
- [Compare](https://xcb.sh/compare): how xcb compares with request routers, agent workspaces, provider coding tools, and managed task environments.
- [xcb vs OpenRouter](https://xcb.sh/compare/openrouter): OpenRouter bills per token for API calls to many models; xcb sends each coding task to a Claude or Codex subscription you already pay for.
- [Routing that learns you](https://xcb.sh/reflexes): learned routing and continuation built from ALGAL programs and local evidence.
- [README as Markdown](https://xcb.sh/README.md): the repository README.
- [Blog](https://xcb.sh/blog): posts on how xcb works, with an [Atom feed](https://xcb.sh/blog/feed.xml).
- [Sitemap](https://xcb.sh/sitemap.xml): indexable HTML pages.

## Blog posts

${blogLines}
`;

export function GET(): Response {
  return new Response(body, {
    headers: {
      "cache-control": "public, max-age=3600",
      "content-type": "text/plain; charset=utf-8",
    },
  });
}
