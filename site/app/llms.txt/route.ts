import { blogPostPath, indexableBlogPosts } from "../blog/posts";
import { publicationMarkdown, publishedRelease } from "../publication";

// llms.txt is a machine-readable site summary. Its release claim comes from
// the same signed-off datum as every public page, so it cannot drift.
export const dynamic = "force-static";

const releaseLine = publishedRelease === null
  ? "No native xcb binary or @hraness/xcb npm package is published yet; the source build is the installation path."
  : `Latest verified release: v${publishedRelease.version}, verified by ${publishedRelease.verificationRun}.`;
const releaseDetails = publicationMarkdown(publishedRelease);
// Only indexable posts are listed; quarantined posts stay out of machine-readable maps.
const blogLines = indexableBlogPosts.map((entry) => `- [${entry.title}](https://xcb.sh${blogPostPath(entry)}): ${entry.dek}`).join("\n");

const body = `# xcb

> xcb routes coding tasks across the Claude, Codex, and Devin subscriptions you already pay for, picking an account that is signed in and idle.

xcb, short for Excalibur, is a terminal and router for developers who pay for more than one coding agent. Type the work into one conversation, and xcb sends each task to a Claude, Codex, or Devin account that is signed in, idle, and not at a known usage limit, on a model that fits the job. Tasks keep running after you close the terminal, every session appears on one screen, and each account runs one task at a time. Other agents can hand xcb work with one JSON command: \`xcb --json route\` reads one JSON task on stdin and writes one JSON result on stdout, and \`dryRun\` previews the route without reserving an account. Applications can embed the TypeScript SDK's \`createSubscriptionRouter\`, which holds the account the application names while its task runs; the application supplies the provider adapters. xcb is MIT licensed, in preview for macOS and Linux. The managed harness is in development, and the current build does not execute self-modifying orchestration policies.

Native xcb is an MIT-licensed Rust source preview. ${releaseLine} Releases tagged v0.3.0 and earlier are AgentMixer. The release pipeline builds native binaries for macOS ARM64 (darwin-aarch64) and Linux x86_64 (linux-x86_64); other hosts build from source, and the updater installs nothing on them. Claude and Codex coding workflows passed on macOS ARM64 with the tested accounts and supported builds. The supported Devin builds pass sandbox checks without signing in; a coding session on a signed-in Devin account has not been confirmed. The native installer is per-user, records its verified install helper, and supports \`xcb update check\`, \`xcb upgrade\`, and a macOS-only daily \`notify\` or opt-in \`auto\` policy; updates install only verified releases, never main or an unverified build. Supported provider builds are listed in the docs.

Managed ALGAL controllers ship in v0.6.0. Use \`xcb backlog program\` to run a controller now or \`xcb schedules program\` for recurring work, and set \`--managed-calls\` to how many worker tasks it may start (1 to 8). A current project grant covers every worker task it starts. A controller pauses while an ordinary routed worker runs, gives up its worker slot while it waits, and resumes only after that worker's task has completed. \`xcb backlog program-status <task-id>\` and \`/program\` show the linked worker task, progress, and run records. Questions and approvals from those workers still come to you as usual. See the [project-agent reference](https://github.com/hraness/xcb/blob/v0.6.0/docs/project-agents.md).

The v0.7.0 terminal adds familiar editing keys and searchable history. Ctrl-T opens saved transcript pages, F3 searches the loaded pages, Ctrl-R searches prompt history, and Ctrl-G opens the draft in your editor. \`/resume\` reopens a conversation and \`/rename\` changes its title. Agent lists and task details update while open; guidance and answers name their target task. \`/drafts\` recovers saved input for review and never resends it automatically. See the [terminal guide](https://github.com/hraness/xcb/blob/v0.7.0/docs/terminal.md).

The v0.8.0 terminal adds a session overview above chat. Each card shows the session name, routed model, activity, and a preview of its latest response, with labels and colors for questions, approvals, completed work, usage limits, and problems. Sessions needing attention come first, then active work, and cards keep their order while responses arrive or you browse. Press F6 to browse the grid; \`1\`, \`2\`, and \`3\` show all sessions, active work, or attention only, and \`/\` or Ctrl-F filters by name, model, status, or ID. Enter, or a card click with \`/mouse\` enabled, adds that session's reference and a response snapshot to your draft without sending anything. The overview holds up to 128 sessions. See the [terminal guide](https://github.com/hraness/xcb/blob/v0.8.0/docs/terminal.md).

A route candidate needs a supported provider build, an enabled, signed-in, idle account outside any known quota window, and a model recently seen in that provider's catalog. Candidates are ranked by task type and by relative quality, cost, and latency (Pareto tiers); an optional judge can inform that ranking but cannot add a route. The workspace broker can read and edit files. A separately configured Linux ARM64 command runner supports offline tests/builds, prepared public Cargo/Bun dependencies, and filtered read-only Git inspection. It does not run native macOS commands, arbitrary network operations, private dependency installs, commits, or pushes. Model requests still go to the selected provider; local state does not mean offline inference. xcb is not an unlimited parallel agent fleet.

${releaseDetails === "" ? "" : `## Verified release\n\n${releaseDetails}\n\n`}## Pages

- [Overview](https://xcb.sh/): the router, the JSON command and SDK, how routes are chosen, and source installation.
- [Download](https://xcb.sh/download): latest verified native release, per-platform archives, and the source build.
- [Compare](https://xcb.sh/compare): how xcb compares with request routers, agent workspaces, provider coding tools, and managed task environments, with links to each product's own site or documentation. xcb is a local router around coding-agent accounts, not a general-purpose multi-agent task graph.
- [Documentation](https://xcb.sh/docs): task-oriented guides and current readiness.
- [Getting started](https://xcb.sh/docs/getting-started): build native xcb and connect an account.
- [Route tasks](https://xcb.sh/docs/route): the JSON route command for agents and the TypeScript SDK entry point.
- [Accounts & models](https://xcb.sh/docs/providers): supported builds, authentication, model selection, quota windows.
- [Tests, builds & recovery](https://xcb.sh/docs/workspace): isolated runner, public dependencies, Git limits, cancellation, recovery.
- [Make it yours](https://xcb.sh/docs/customization): panes, continuation, context management, and optional extensions.
- [Learned routing & continuation](https://xcb.sh/docs/reflexes): reflexes that learn the model tier you want and when a worker stopped short, with promotion only after a trial on new labels, and rollback. In v0.6.0, settle and confirm default to auto and act only once your own replies certify them. Earlier v0.5.0 downloads default both to observe, require explicit active settings for them to act, and do not accept auto; see the [v0.5.0 reflex reference](https://github.com/hraness/xcb/blob/v0.5.0/docs/reflexes.md).
- [Routing that learns you](https://xcb.sh/reflexes): the use case: learned routing and continuation built from ALGAL programs and local evidence.
- [Application API](https://xcb.sh/docs/application-api): one model response per call for your app, with no tools or saved history, once your exact build, account, and model pass xcb's application checks; separate from coding sessions.
- [Reference](https://xcb.sh/docs/reference): full project README and compatibility source reference.
- [README as Markdown](https://xcb.sh/README.md): machine-readable project contract.
- [Blog](https://xcb.sh/blog): posts on how xcb works and how it uses other Hraness tools, with an [Atom feed](https://xcb.sh/blog/feed.xml).
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
