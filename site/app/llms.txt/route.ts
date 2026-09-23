import { publicationMarkdown, publishedRelease } from "../publication";

// llms.txt is a machine-readable site summary. Its release claim comes from
// the same signed-off datum as every public page, so it cannot drift.
export const dynamic = "force-static";

const releaseLine = publishedRelease === null
  ? "No native xcb binary or @hraness/xcb npm package is published yet; the source build is the installation path."
  : `Latest verified release: v${publishedRelease.version}, verified by ${publishedRelease.verificationRun}.`;
const releaseDetails = publicationMarkdown(publishedRelease);

const body = `# xcb

> One router for the AI coding subscriptions you already pay for. xcb (Excalibur) multiplexes whole tasks across your Claude, Codex, and Devin accounts — it selects an eligible account/model route, runs one bounded turn, and proves account custody when the work settles.

Another coding agent calls \`xcb --json route\`: one closed JSON task document on stdin, one bounded JSON result on stdout, with \`dryRun\` for selection without reserving an account. Applications embed the TypeScript SDK's \`createSubscriptionRouter\`, which wraps the account lease store and host-qualified task adapters. A terminal workspace is the reference host; the managed harness is experimental and is being rebuilt as a self-evolving ALGAL harness — routing manifests are proposed, evaluated on labeled cases, and promoted only when strictly better, while admission, custody, and settlement contracts stay fixed. The current build does not execute self-modifying orchestration policies.

Native xcb is an MIT-licensed Rust source preview. ${releaseLine} Releases tagged v0.3.0 and earlier are AgentMixer. Native release binaries are built for macOS ARM64 (darwin-aarch64) and Linux x86_64 (linux-x86_64); other hosts build from source, and the updater fails closed elsewhere. Claude and Codex coding workflows passed on macOS ARM64 with tested accounts and admitted builds. Devin's exact admitted builds pass credential-free boundary checks; authenticated coding acceptance requires separate account, model, and build evidence. The native installer is user-global, records its verified install helper, and supports \`xcb update check\`, \`xcb upgrade\`, and a macOS-only daily \`notify\` or opt-in \`auto\` policy; no update follows main or an unverified moving artifact. Provider builds and host qualification have explicit limits.

Routing candidates must be admitted runtimes on enabled, credentialed, idle accounts with observed fresh model entries, outside known quota windows; ranking uses task class and relative quality/cost/latency Pareto tiers, with an optional judge ordering only already-eligible routes. The workspace broker can read and edit files. A separately configured Linux ARM64 command runner supports offline tests/builds, prepared public Cargo/Bun dependencies, and filtered read-only Git inspection. It does not run native macOS commands, arbitrary network operations, private dependency installs, commits, or pushes. Model requests still go to the selected provider; local state does not mean offline inference. xcb is not an unlimited parallel agent fleet.

${releaseDetails === "" ? "" : `## Verified release\n\n${releaseDetails}\n\n`}## Pages

- [Overview](https://xcb.sh/): the subscription router, its two contracts, route guarantees, and source installation.
- [Download](https://xcb.sh/download): latest verified native release, per-platform archives, and the source build.
- [Compare](https://xcb.sh/compare): product-focus comparison with Claude Code, Codex CLI, OpenCode, and Devin cloud, with primary sources. xcb is a local router around coding-agent accounts, not a general-purpose multi-agent task graph.
- [Documentation](https://xcb.sh/docs): task-oriented guides and current readiness.
- [Getting started](https://xcb.sh/docs/getting-started): build native xcb and connect an account.
- [Route tasks](https://xcb.sh/docs/route): the agent-facing JSON route contract and the TypeScript SDK entry point.
- [Accounts and providers](https://xcb.sh/docs/providers): supported builds, authentication, model selection, quota windows.
- [Workspace commands](https://xcb.sh/docs/workspace): isolated runner, public dependencies, Git limits, cancellation, recovery.
- [Customization](https://xcb.sh/docs/customization): sessions, panes, and optional behavior.
- [Learned routing & continuation](https://xcb.sh/docs/reflexes): reflexes that learn the model tier you want and when a worker stopped short, with forward-trial promotion and rollback.
- [Routing that learns you](https://xcb.sh/reflexes): the use case: learned routing and continuation built from ALGAL programs and local evidence.
- [Application API](https://xcb.sh/docs/application-api): ephemeral text generation with exact qualification; separate from coding sessions.
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
