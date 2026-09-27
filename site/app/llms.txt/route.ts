import { blogPostPath, indexableBlogPosts } from "../blog/posts";
import { providerStatus, supportedBuilds } from "../docs/provider-status";
import { docsTopics } from "../docs/topics";
import { publicationMarkdown, publishedRelease } from "../publication";

// llms.txt is a machine-readable site summary. Its release claim comes from
// the same signed-off datum as every public page, so it cannot drift.
export const dynamic = "force-static";

const releaseLine = publishedRelease === null
  ? "No native xcb binary or @hraness/xcb package archive is published yet; build from source."
  : "Verified release archives cover macOS ARM64 (darwin-aarch64) and Linux x86_64 (linux-x86_64); other hosts build from source.";
const releaseDetails = publicationMarkdown(publishedRelease);
const docsLines = docsTopics.map((topic) => `- [${topic.title}](https://xcb.sh/docs/${topic.slug}): ${topic.description}`).join("\n");
// Only indexable posts are listed; quarantined posts stay out of machine-readable maps.
const blogLines = indexableBlogPosts.map((entry) => `- [${entry.title}](https://xcb.sh${blogPostPath(entry)}): ${entry.dek}`).join("\n");

const body = `# xcb

> xcb routes coding tasks across the Claude, Codex, and Devin subscriptions you already pay for.

xcb, short for Excalibur, is a terminal and router for developers who pay for more than one coding agent. Each task runs on an account that is signed in, idle, and not at a known usage limit, on a model that fits the work, and xcb holds that account until the provider process exits. It is MIT licensed. ${releaseLine}

## Use it as a coding agent

Plain \`xcb\` opens one thread for all your projects. xcb picks each task's project folder and says why, then picks an account and model. Tasks keep running after you close the terminal, and a turn that stops at a usage limit continues on another account or model. Set up with \`xcb setup claude\` or \`xcb setup codex\`; Devin connects by importing the Devin CLI's sign-in.

## Build on it

- Agents and scripts: \`xcb --json route\` reads one JSON task on stdin (\`version\`, \`workspace\`, \`task\`, optional \`provider\`, \`account\`, \`model\`, \`timeoutMs\`, \`dryRun\`), picks the account and model, runs one turn, and prints one JSON result. It exits 0 only for a completed turn; failures carry a \`code\` such as \`unavailable\`, \`busy\`, \`needs_input\`, or \`custody_unproven\`.
- TypeScript apps: \`createSubscriptionRouter\` from the \`@hraness/xcb\` SDK runs a task on the account and model the app names and holds that account until the provider exits. It does not choose the account or model. The SDK ships as a release archive, not on npm.
- Apps that need one tool-free model response: \`xcb --json generate\`, after the build, account, and model pass xcb's application checks.

## Providers and limits

- Claude: Claude Code ${supportedBuilds.claudeMinimum} or later within version 2. ${providerStatus.claude} On Linux, Claude runs after xcb's sandbox checks pass on that machine.
- Codex: Codex CLI ${supportedBuilds.codex.join(", ")} on macOS ARM64. ${providerStatus.codex}
- Devin: Devin CLI ${supportedBuilds.devin.join(", ")} on macOS ARM64. ${providerStatus.devin}
- Providers work through xcb's file tools in an OS sandbox, without their own shells or plugins. Model requests go from each provider's CLI to that provider.
- The command runner for tests and builds is an offline Linux VM on macOS ARM64. Git is read-only there, and native macOS builds can't run.
- Each account runs one provider turn at a time, and tasks in the same project folder take turns.
- The self-tuning managed harness is in development; the current build does not run self-modifying routing policies.

${releaseDetails === "" ? "" : `## Verified release\n\n${releaseDetails}\n\n`}## Documentation

${docsLines}

## Pages

- [Overview](https://xcb.sh/): what xcb does, how it routes, and how to install it.
- [Download](https://xcb.sh/download): native archives, checksums, and the source build.
- [Documentation](https://xcb.sh/docs): all guides, grouped for using xcb and building on it.
- [Compare](https://xcb.sh/compare): how xcb compares with request routers, agent workspaces, provider coding tools, and managed task environments.
- [xcb vs OpenRouter](https://xcb.sh/compare/openrouter): OpenRouter bills per token for API calls to many models; xcb sends each coding task to a Claude, Codex, or Devin subscription you already pay for.
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
