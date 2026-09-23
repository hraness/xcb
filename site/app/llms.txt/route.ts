import { publicationMarkdown, publishedRelease } from "../publication";

// llms.txt is a machine-readable site summary. Its release claim comes from
// the same signed-off datum as every public page, so it cannot drift.
export const dynamic = "force-static";

const releaseLine = publishedRelease === null
  ? "No native xcb binary or @hraness/xcb npm package is published yet; the source build is the installation path."
  : `Latest verified release: v${publishedRelease.version}, verified by ${publishedRelease.verificationRun}.`;
const releaseDetails = publicationMarkdown(publishedRelease);

const body = `# xcb

> xcb (Excalibur) routes coding tasks across the Claude, Codex, and Devin subscriptions you already pay for. For each task it picks one of your accounts that is signed in, idle, and not at a known quota limit, and keeps that account locked until the provider process has exited.

Another coding agent calls \`xcb --json route\`: it writes one JSON task to stdin and reads one JSON result from stdout, and \`dryRun\` previews the route without reserving an account. Applications can embed the TypeScript SDK's \`createSubscriptionRouter\`, which holds the account the application names while its task runs; the application supplies the provider adapters. xcb's own terminal workspace is built on the same router. The managed harness is experimental. It is being rebuilt on ALGAL to propose routing rules, test them on labeled examples, and keep only rules that score strictly better, while provider checks, account locking, and run records stay fixed. The current build does not execute self-modifying orchestration policies.

Native xcb is an MIT-licensed Rust source preview. ${releaseLine} Releases tagged v0.3.0 and earlier are AgentMixer. The release pipeline builds native binaries for macOS ARM64 (darwin-aarch64) and Linux x86_64 (linux-x86_64); other hosts build from source, and the updater installs nothing on them. Claude and Codex coding workflows passed on macOS ARM64 with the tested accounts and supported builds. The supported Devin builds pass sandbox checks without signing in; a coding session on a signed-in Devin account has not been confirmed. The native installer is per-user, records its verified install helper, and supports \`xcb update check\`, \`xcb upgrade\`, and a macOS-only daily \`notify\` or opt-in \`auto\` policy; updates install only verified releases, never main or an unverified build. Supported provider builds are listed in the docs.

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
- [Learned routing & continuation](https://xcb.sh/docs/reflexes): reflexes that learn the model tier you want and when a worker stopped short, with promotion only after a trial on new labels, and rollback.
- [Routing that learns you](https://xcb.sh/reflexes): the use case: learned routing and continuation built from ALGAL programs and local evidence.
- [Application API](https://xcb.sh/docs/application-api): one model response per call for your app, with no tools or saved history, once your exact build, account, and model pass xcb's application checks; separate from coding sessions.
- [Reference](https://xcb.sh/docs/reference): full project README and compatibility source reference.
- [README as Markdown](https://xcb.sh/README.md): machine-readable project contract.
- [Sitemap](https://xcb.sh/sitemap.xml): indexable HTML pages.
`;

export function GET(): Response {
  return new Response(body, {
    headers: {
      "cache-control": "public, max-age=3600",
      "content-type": "text/plain; charset=utf-8",
    },
  });
}
