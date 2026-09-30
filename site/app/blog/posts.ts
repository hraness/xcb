import type {
  ArticleAdmission,
  ArticleAuthor,
  ArticleIsoDate,
  ArticleReview,
  ArticleSourceItem,
} from "@hraness/design-kit";

import { launchPostSlug } from "../launch/beats";

/**
 * The xcb blog registry. Each post's body lives in `content/blog/<slug>.md`
 * and is rendered by `scripts/sync-blog.ts`; this file owns titles, dates,
 * sources, and the admission record that decides whether a post is indexed.
 *
 * A quarantined post is readable at its URL but ships `noindex` and stays out
 * of the blog index, the sitemap, the Atom feed, and llms.txt.
 */

export const blogPath = "/blog";
export const blogFeedPath = "/blog/feed.xml";
export const blogTitle = "Excalibur (xcb) blog";
export const blogDescription = "How xcb routes coding tasks across your Claude, Codex, and Devin subscriptions, and how it uses other Hraness tools to do it.";

export const blogAuthor: ArticleAuthor = { kind: "organization", name: "Hraness" };

const reviewer = "Claude Opus 5.5 (claude-opus-5-5) editorial review";
const reviewedOn: ArticleIsoDate = "2026-09-27";
const checkedOn: ArticleIsoDate = "2026-09-24";

/**
 * The AI editorial review on record for the posts published on 2026-09-24.
 * Each post names its own review; a post nobody has reviewed yet passes null
 * and stays quarantined.
 */
const editorialReview: ArticleReview = { reviewer, reviewerType: "ai", reviewedOn };

/**
 * The launch post's review, by a different run from the one that drafted it:
 * it checked each claim against source at 27dc148, the launch pull requests,
 * and live runs on 2026-09-27, and edited the Devin status after the live
 * Devin coding runs on the launch build.
 */
const launchReview: ArticleReview = {
  reviewer: "Claude Opus 5.5 (claude-opus-5-5) independent editorial review",
  reviewerType: "ai",
  reviewedOn: "2026-09-27",
};

/** Sources pinned to the commits the fact check read. */
const xcb = (path: string) => `https://github.com/hraness/xcb/blob/6437bcb/${path}`;
const algalAt = (rev: string, path: string) => `https://github.com/hraness/algal/blob/${rev}/${path}`;
const gobstopperAt = (rev: string, path: string) => `https://github.com/hraness/gobstopper/blob/${rev}/${path}`;
const wordcell = (path: string) => `https://github.com/hraness/wordcell/blob/7b6cb5e/${path}`;
const designKitPortfolio = "https://github.com/hraness/design-kit/blob/v0.17.0/src/portfolio.generated.json";
const xcbAt = (rev: string, path: string) => `https://github.com/hraness/xcb/blob/${rev}/${path}`;

/**
 * The launch beats' review, by a different agent from the one that drafted
 * them: a read-only check of each beat against the sources below at 4d94782
 * and the live page on 2026-09-30. It scored the post 9 of 12 with no zero;
 * its platform, quota, and source fixes are applied, which it rated as raising
 * factual confidence to 2.
 */
const beatsReview: ArticleReview = {
  reviewer: "Claude Opus 5.5 (claude-opus-5-5) independent editorial review",
  reviewerType: "ai",
  reviewedOn: "2026-09-30",
};
const beatsAt = (path: string) => xcbAt("4d94782", path);
const beatsCheckedOn: ArticleIsoDate = "2026-09-30";

/** Introducing Excalibur's fact check read xcb at 27dc148 and the two external sites on this date. */
const launch = (path: string) => xcbAt("27dc148", path);
const launchCheckedOn: ArticleIsoDate = "2026-09-27";

export type BlogPost = Readonly<{
  slug: string;
  title: string;
  dek: string;
  eyebrow: string;
  published: ArticleIsoDate;
  keywords: readonly string[];
  /**
   * The registered portfolio relation the post expands, when it is a
   * "How xcb uses X" post. Related products render only from this relation.
   * `"all"` shows every registered xcb relation (the introduction).
   */
  relation: string | "all" | null;
  /** True when the body carries the `{{release.version}}` status sentence itself. */
  statusInBody: boolean;
  /**
   * `"beats"` for a launch post built from `app/launch/beats.ts` with its own
   * route; every other post is Markdown in `content/blog/<slug>.md`.
   */
  format?: "beats";
  sources: readonly ArticleSourceItem[];
  admission: ArticleAdmission;
}>;

function sourceRecords(sources: readonly ArticleSourceItem[]) {
  return sources.map(({ title, href, checkedOn: checked }) => ({ title, url: href, checkedOn: checked }));
}

function post(
  entry: Omit<BlogPost, "admission"> & Readonly<{
    admission: Omit<ArticleAdmission, "href" | "sources" | "drafting" | "humanReview" | "owner">;
  }>,
): BlogPost {
  return {
    ...entry,
    admission: {
      ...entry.admission,
      href: `${blogPath}/${entry.slug}`,
      sources: sourceRecords(entry.sources),
      owner: "Hraness",
      drafting: "ai-from-source",
      humanReview: null,
    },
  };
}

export const blogPosts: readonly BlogPost[] = [
  post({
    slug: launchPostSlug,
    title: "Introducing Excalibur: one agent for all your AI coding plans",
    dek: "Bite-size posts on what xcb does for people who pay for more than one of Claude, Codex, and Devin, each with its own illustration.",
    eyebrow: "Launch",
    published: "2026-09-29",
    keywords: ["xcb", "Excalibur", "Claude Code", "Codex", "Devin", "multiple AI subscriptions", "usage limits", "coding agents"],
    relation: "all",
    statusInBody: true,
    format: "beats",
    sources: [
      { title: "xcb README: providers, how routing works, everyday commands, limits", href: beatsAt("README.md"), checkedOn: beatsCheckedOn },
      { title: "Quota routing: fresh usage within five minutes, Claude and Codex windows, failover on a reported limit", href: beatsAt("docs/quota-routing.md"), checkedOn: beatsCheckedOn },
      { title: "Remote operations: xcb link, fleet, dispatch, end-to-end encrypted task content, your own relay", href: beatsAt("docs/remote-operations.md"), checkedOn: beatsCheckedOn },
      { title: "Route contract: the JSON request and result", href: beatsAt("docs/route.md"), checkedOn: beatsCheckedOn },
      { title: "Launch facts and beats", href: beatsAt("site/app/launch/facts.ts"), checkedOn: beatsCheckedOn },
      { title: "Published release record", href: beatsAt("site/published-release.json"), checkedOn: beatsCheckedOn },
    ],
    admission: {
      lifecycle: "indexable",
      readerJob: "Decide in a minute whether xcb is for you when you pay for more than one AI coding plan, and share the one piece that makes the case.",
      nonObviousAnswer: "xcb does not add capacity; it spends the quota you already have in the right order, favoring unused quota close to a reset, and moves a task to another account when a provider reports a limit mid-task.",
      originalContribution: "Ten standalone claims, each checked against xcb's README and docs at the pinned commit and paired with an illustration drawn from the CLI's own output shapes, which the social posts are cut from without rewording.",
      hostFit: "The product's own launch post on its own host, with the long introduction at /blog/introducing-excalibur as its technical companion.",
      nearestUrls: [
        { url: "/blog/introducing-excalibur", distinction: "The introduction follows one task through sign-in, sandbox, and recorded outcome in long form; this post gives the same product as short standalone claims for a first look and for sharing." },
        { url: "/", distinction: "The home page answers setup questions and installs; this post makes the case one claim at a time." },
      ],
      observations: [
        "Every number in the post comes from app/launch/facts.ts, and tests read the README and docs to check each one.",
        "The illustrations reuse the exact line shapes `xcb accounts`, `xcb tasks`, `xcb attention`, and `xcb fleet` print, and a test fails if a shape drifts from the CLI source.",
      ],
      scores: { readerUtility: 2, originalEvidence: 1, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 1 },
      review: beatsReview,
      reassessOn: "2026-11-10",
      harmIfWrong: "A reader could expect xcb to add usage, to keep provider plugins inside its runs, or to move every task on any limit, and pay for a plan they would not use.",
      refreshTriggers: [
        "xcb release tag bump (site/published-release.json)",
        "A change to quota routing freshness, the windows xcb reads, or failover on a reported limit",
        "A change to xcb link, fleet, dispatch, or relay encryption",
        "A change to the output shapes of xcb accounts, tasks, attention, or fleet",
      ],
    },
  }),
  post({
    slug: "introducing-excalibur",
    title: "Introducing Excalibur",
    dek: "xcb, short for Excalibur, sends each coding task to one of your Claude, Codex, or Devin accounts that is signed in, idle, and not at a known usage limit.",
    eyebrow: "Release",
    published: "2026-09-27",
    keywords: ["xcb", "Excalibur", "coding agents", "routing", "Claude Code", "Codex", "Devin"],
    relation: "all",
    statusInBody: true,
    sources: [
      { title: "xcb README: lead, xcb setup, the thread, readiness by provider, the command runner, optional behavior", href: launch("README.md"), checkedOn: launchCheckedOn },
      { title: "Route contract: request fields, the completed response, failure codes, the 256 KiB text cap", href: launch("docs/route.md"), checkedOn: launchCheckedOn },
      { title: "Managed harness: choosing a task's directory, the detached supervisor, concurrency, a new route with the original instructions", href: launch("docs/managed-harness.md"), checkedOn: launchCheckedOn },
      { title: "Quota routing: Claude's five-hour and seven-day windows, no inferred Codex or Devin limits, failover after a reported quota failure", href: launch("docs/quota-routing.md"), checkedOn: launchCheckedOn },
      { title: "Provider launches: a private copy of the executable, private home, cleared environment, Seatbelt and bwrap sandboxes, provider tools and MCP off", href: launch("crates/xcb-runtime/src/runner.rs"), checkedOn: launchCheckedOn },
      { title: "Claude sign-in through claude setup-token in a private profile", href: launch("crates/xcb-runtime/src/auth.rs"), checkedOn: launchCheckedOn },
      { title: "Codex configuration: web search and agents turned off", href: launch("crates/xcb-runtime/src/codex/config.rs"), checkedOn: launchCheckedOn },
      { title: "Supported Codex and Devin builds", href: launch("qualified-builds.json"), checkedOn: launchCheckedOn },
      { title: "xcb setup: add or reuse an account, check the provider build, sign in, load models", href: launch("crates/xcb-cli/src/main.rs"), checkedOn: launchCheckedOn },
      { title: "Hooks: stored disabled, pinned by SHA-256, run with a cleared environment", href: launch("crates/xcb-runtime/src/hooks.rs"), checkedOn: launchCheckedOn },
      { title: "Reflexes reference: learning from your replies, rollback, effect-free programs", href: launch("docs/reflexes.md"), checkedOn: launchCheckedOn },
      { title: "Docs site: panes as presentation data, the TypeScript SDK's createSubscriptionRouter", href: launch("site/app/docs/topic-content.tsx"), checkedOn: launchCheckedOn },
      { title: "Home page: the managed harness rebuild and what stays fixed", href: launch("site/app/page.tsx"), checkedOn: launchCheckedOn },
      { title: "Published release record", href: launch("site/published-release.json"), checkedOn: launchCheckedOn },
      { title: "Herdr home page: coding agents in their own terminals, marked working, blocked, or idle", href: "https://herdr.dev/", checkedOn: launchCheckedOn },
      { title: "Pi home page: a minimal agent harness you adapt with extensions", href: "https://pi.dev/", checkedOn: launchCheckedOn },
      { title: "Launch fixes and live runs: Devin 3000.11.3 route and managed coding tasks on macOS ARM64", href: "https://github.com/hraness/xcb/pull/247", checkedOn: launchCheckedOn },
    ],
    admission: {
      lifecycle: "indexable",
      readerJob: "Decide whether xcb is worth installing today when you pay for more than one of Claude, Codex, and Devin or build agent tooling, and know the first commands to run.",
      nonObviousAnswer: "xcb runs each task through a private copy of the provider's own tool under your sign-in, holds one account per task until the provider process exits, and sandboxes the run. The route command picks the account and model, while an SDK host names both. Panes and reflexes cannot widen what a run may do, and hooks run as you, so each starts off. With the tested accounts, Claude and Devin have completed coding tasks on macOS on Apple silicon; Linux runs only Claude, and Codex's last signed-in coding run used the previous supported build.",
      originalContribution: "One account of how xcb runs a task, the thread and the route and SDK uses, where it sits beside Herdr and Pi, and its limits, with each claim taken from xcb source at 27dc148 and from the two tools' own sites rather than from the home page.",
      hostFit: "The product's own introduction on its own host. It replaces Introducing xcb, whose URL redirects here, for the v0.10.0 launch.",
      nearestUrls: [
        { url: "/docs/getting-started", distinction: "The guide gives install and sign-in steps for each provider; the post explains what xcb does, who it suits, and its limits, and ends with the shortest start." },
        { url: "/", distinction: "The home page lists features and answers setup questions; the post follows one task from sign-in to its recorded outcome and keeps the limits in one place." },
        { url: "/compare", distinction: "The comparison pages weigh xcb against each tool in detail; the post gives a first-time reader the short version and links there." },
      ],
      observations: [
        "The provider process never runs in the project directory: xcb launches a private copy of the binary in a scratch directory with its own home, and project files are reachable only through xcb's file tools, which is why MCP servers, plugins, and web search are unavailable inside xcb runs (runner.rs, codex/config.rs).",
        "Hooks are off twice by default: the hooks extension is disabled in config.json and each new hook is stored disabled; a hook whose executable changes fails with \"register it again\" until it is added anew (hooks.rs, config.rs).",
        "xcb setup claude needs no --plan flag: the label defaults to \"Subscription\", is display-only, and an existing enabled Claude account is reused rather than duplicated (main.rs).",
      ],
      scores: { readerUtility: 2, originalEvidence: 1, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 1 },
      review: launchReview,
      reassessOn: "2026-11-08",
      harmIfWrong: "A reader could install xcb expecting Linux coding sessions or the newest Codex build to work today, run several accounts believing xcb raises usage limits, or enable a hook believing it runs inside xcb's sandbox.",
      refreshTriggers: [
        "xcb release tag bump (site/published-release.json), including the v0.10.0 datum",
        "A change to which providers have a confirmed coding session, or to the supported Devin builds: update the Limits paragraph",
        "A change to the supported Codex or Devin builds (qualified-builds.json) or the Claude Code version floor, or a coding session confirmed for Claude on Linux or on the current Codex build",
        "The one-line installer at /install.sh shipping, changing its platforms, or being withdrawn, or a change to xcb setup",
        "A change to the route request or response, its failure codes, or the 256 KiB text cap, or to how the SDK's createSubscriptionRouter chooses accounts",
        "The /compare, /compare/herdr, or /compare/pi pages moving or changing their verdicts, or Herdr or Pi changing what their own sites say they do",
        "The managed harness leaving experimental status or running self-tuning routing, or a change to panes, hooks, or reflex rollback",
        "A change to a registered relation detail for xcb, or a product rename",
      ],
    },
  }),
  post({
    slug: "how-xcb-uses-gobstopper",
    title: "How xcb uses Gobstopper to trim stale tool output",
    dek: "Past a size threshold, xcb uses Gobstopper to swap old tool output in Claude Code and Codex prompts for a short marker, keeping the original locally.",
    eyebrow: "Integration",
    published: "2026-09-24",
    keywords: ["xcb", "Gobstopper", "context management", "coding agents", "Claude Code", "Codex"],
    relation: "runtime:gobstopper:xcb:compacts-sessions-for",
    statusInBody: true,
    sources: [
      { title: "xcb prompt compaction: Gobstopper elide plan, protected tail, optional judge veto, elision marker", href: xcb("crates/xcb-runtime/src/context.rs"), checkedOn },
      { title: "xcb runtime dependency pin: gobstopper-core 0.2.1 at rev d856594", href: xcb("crates/xcb-runtime/Cargo.toml"), checkedOn },
      { title: "xcb context policy defaults and validation", href: xcb("crates/xcb-runtime/src/config.rs"), checkedOn },
      { title: "xcb turn runner: fallback to the deterministic plan and the elision notice", href: xcb("crates/xcb-runtime/src/runner.rs"), checkedOn },
      { title: "xcb README: optional behavior, on by default, plugins disable gobstopper", href: xcb("README.md"), checkedOn },
      { title: "xcb compatibility guide: judge veto of Gobstopper elision", href: xcb("docs/compatibility.md"), checkedOn },
      { title: "Gobstopper README: why, recoverable history, strategies", href: gobstopperAt("60e4c9e", "README.md"), checkedOn },
      { title: "Gobstopper elide strategy at the pinned rev", href: gobstopperAt("d856594", "crates/gobstopper-core/src/strategy/elide.rs"), checkedOn },
      { title: "Registered relation detail", href: designKitPortfolio, checkedOn },
    ],
    admission: {
      lifecycle: "indexable",
      readerJob: "Understand what xcb does to a long Claude Code or Codex prompt with Gobstopper on, and how to tune or disable it.",
      nonObviousAnswer: "xcb uses only Gobstopper's model-free elide rule, in memory: it stubs old tool results in the outgoing prompt, never user or assistant text or the last eight messages, and leaves the saved history untouched; the optional judge can only keep outputs and never sees their contents.",
      originalContribution: "The selection rule, defaults, marker text, and judge limits read from xcb's context.rs, config.rs, and runner.rs and Gobstopper's elide strategy at the pinned revision.",
      hostFit: "A How xcb uses Gobstopper post on xcb's host for the registered relation runtime:gobstopper:xcb:compacts-sessions-for, expanding its detail sentence.",
      nearestUrls: [
        { url: "/docs/customization", distinction: "The guide lists the setting; the post explains which output is trimmed, which is kept, and why." },
        { url: "https://gobstopper.sh", distinction: "Gobstopper's own site covers the standalone tool and its archive, which xcb does not use on this path." },
      ],
      observations: [
        "On this path xcb calls gobstopper-core's elide strategy in memory and keeps originals in its own session history; it uses neither the Gobstopper program nor Gobstopper's archive.",
        "Devin sessions are sent without elision because context.rs maps the Devin provider to no Gobstopper provider.",
      ],
      scores: { readerUtility: 2, originalEvidence: 2, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 1 },
      review: editorialReview,
      reassessOn: "2026-11-05",
      harmIfWrong: "A reader could expect elided output to be gone for good, or expect Devin sessions to be trimmed, and size their sessions on a wrong assumption.",
      refreshTriggers: [
        "Change to the relation detail runtime:gobstopper:xcb:compacts-sessions-for",
        "gobstopper-core rev bump in crates/xcb-runtime/Cargo.toml",
        "Change to ContextPolicy defaults or validation in config.rs, or to selection rules, marker text or judge rules in context.rs",
        "Change to the elision notice or judge fallback in runner.rs",
        "Devin sessions gaining compaction",
        "xcb release tag bump",
        "Rename of xcb or Gobstopper",
      ],
    },
  }),
  post({
    slug: "replayable-task-history",
    title: "How to check an xcb task history offline",
    dek: "xcb tasks verify replays a task's recorded history on your own machine, with no network, and fails if a record was edited or a step is missing.",
    eyebrow: "Technique",
    published: "2026-09-24",
    keywords: ["xcb", "replay", "verification", "coding agents", "ALGAL"],
    relation: null,
    statusInBody: false,
    sources: [
      { title: "Task verifier: walks the chain from the stored task back to revision 1 and compares each replayed entry to the stored record", href: xcb("crates/xcb-runtime/src/managed.rs"), checkedOn },
      { title: "Transition program: the one-step ALGAL program every task revision passes through", href: xcb("crates/xcb-runtime/managed-transition.algal.json"), checkedOn },
      { title: "Test: a task still verifies after the state database is reopened and upgraded", href: xcb("crates/xcb-runtime/src/managed_habitat_tests.rs"), checkedOn },
      { title: "Scheduled programs: pinned program content is re-checked, and edited checkpoints cannot resume", href: xcb("crates/xcb-runtime/src/managed_program.rs"), checkedOn },
      { title: "Contract test: automatic continuation never fires for uncertain, cancelled, completed, or pending-attention turns", href: xcb("crates/xcb-core/tests/contracts.rs"), checkedOn },
      { title: "CLI: xcb tasks verify and the global --state option", href: xcb("crates/xcb-cli/src/main.rs"), checkedOn },
      { title: "README: the verifier replays the task's local chain and does not confirm provider claims or real-world outcomes", href: xcb("README.md"), checkedOn },
      { title: "Managed harness docs, Verification section", href: xcb("docs/managed-harness.md"), checkedOn },
    ],
    admission: {
      lifecycle: "indexable",
      readerJob: "Confirm that an xcb managed task's local history is complete and unedited before relying on what the agent reported.",
      nonObviousAnswer: "Run `xcb tasks verify` against a copy of the state directory: it replays every revision through ALGAL offline and fails on an edited or missing entry, but it is a consistency check with no signature, so it cannot catch a fully rebuilt history or judge the work itself.",
      originalContribution: "The verifier's four rules and its failure cases read from managed.rs and its tests, with a reader-side procedure (verify a copy) the docs do not state.",
      hostFit: "A product-specific technique post on xcb's host about a command xcb ships.",
      nearestUrls: [
        { url: "https://hraness.com/reference/correctness/verifying-receipts-offline", distinction: "The general technique on hraness.com; this post is the xcb-specific version with its command and limits." },
        { url: "/docs/reference", distinction: "The reference lists the command; the post explains what a pass does and does not show." },
      ],
      observations: [
        "Opening a state directory can upgrade its schema and run retention, so verifying the only copy of a state directory is itself a write; the post tells the reader to verify a copy.",
        "The chain's first link is the literal placeholder sha256:pending, which lets the verifier distinguish a history truncated in the middle from one that starts at revision 1.",
      ],
      scores: { readerUtility: 2, originalEvidence: 2, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 1 },
      review: editorialReview,
      reassessOn: "2026-11-05",
      harmIfWrong: "A reader could treat a passing check as proof the agent's work is right, or run it against their only state directory.",
      refreshTriggers: [
        "xcb release tag bump (site/published-release.json)",
        "Change to the task verifier, the transition program, or the tasks verify JSON output",
        "Change to the 1,024-revision cap, the 30-day retention window, or retention exemptions",
        "ALGAL pin change in crates/xcb-runtime/Cargo.toml",
        "Managed harness leaves experimental status or README verify limits change",
        "Rename of xcb, ALGAL, or the tasks verify command",
      ],
    },
  }),
  post({
    slug: "how-xcb-uses-algal",
    title: "How xcb uses ALGAL for reflexes your replies must certify",
    dek: "xcb's continuation reflexes run as small ALGAL programs that, by default, act only after your own replies certify them, leaving about one turn in ten to you.",
    eyebrow: "Integration",
    published: "2026-09-24",
    keywords: ["xcb", "ALGAL", "reflexes", "coding agents", "routing", "replay"],
    relation: "runtime:xcb:algal:replays-task-history-with",
    statusInBody: false,
    sources: [
      { title: "Reflexes reference: shape, learning, forward trials, auto certification, safety rules, measured history", href: xcb("docs/reflexes.md"), checkedOn },
      { title: "Shipped reflex program (no effects, zero agent calls)", href: xcb("crates/xcb-runtime/reflexes/settle.algal.json"), checkedOn },
      { title: "Reflex runtime: pure runs, program fingerprint recorded with every observation, no prompt text stored", href: xcb("crates/xcb-runtime/src/reflex.rs"), checkedOn },
      { title: "Pinned ALGAL planners and resumable controllers", href: xcb("crates/xcb-runtime/src/managed_program.rs"), checkedOn },
      { title: "ALGAL dependency pinned by commit (algal 0.2.0)", href: xcb("crates/xcb-runtime/Cargo.toml"), checkedOn },
      { title: "xcb README: managed harness is experimental", href: xcb("README.md"), checkedOn },
      { title: "ALGAL README: organisms, declared limits, records that explain a run without calling the model", href: algalAt("1bc117d", "README.md"), checkedOn },
    ],
    admission: {
      lifecycle: "indexable",
      readerJob: "Decide whether to let xcb's reflexes continue stopped turns and answer go-ahead requests for you, and know what stops them from acting on a risky turn.",
      nonObviousAnswer: "Each reflex is an effect-free ALGAL program with zero model calls, so every past decision can be recomputed from its recorded program fingerprint and params version; in auto mode a head acts only after at least 30 firings on your own newest 1,500 labeled turns clear a 99% lower bound of 0.75 (continue) or 0.85 (yes), about one turn in ten stays with you, and the risk veto lives in xcb, outside any program you can swap in.",
      originalContribution: "Certification constants, program budgets, and the veto list read from xcb source at 6437bcb.",
      hostFit: "The registered runtime:xcb:algal:replays-task-history-with relation carries the detail sentence this post explains, on the consumer's host.",
      nearestUrls: [
        { url: "/docs/reflexes", distinction: "The reference documents every rule; the post explains why the program and its params are separate." },
        { url: "/reflexes", distinction: "The use-case page sells the outcome; the post shows the certification rule and its limits." },
      ],
      observations: [
        "Labels from turns xcb answered itself train a head but cannot certify it, because they would only confirm its own choices.",
        "The risk veto lives in xcb rather than in the replaceable program, so a custom reflex program cannot remove it.",
        "The September 26 edit dropped \"continuation\" from the dek, which then said every reflex waits for certification; the route reflex defaults to active (docs/reflexes.md), so the September 27 fact review restored the qualifier.",
      ],
      scores: { readerUtility: 2, originalEvidence: 1, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 1 },
      review: editorialReview,
      reassessOn: "2026-11-05",
      harmIfWrong: "A reader could let reflexes answer go-ahead requests on the belief that a certificate guarantees a correct call.",
      refreshTriggers: [
        "Registration, change or removal of relation runtime:xcb:algal:replays-task-history-with or its detail sentence",
        "xcb release record bump or a move of the ALGAL pin in crates/xcb-runtime/Cargo.toml",
        "Change to certification constants in crates/xcb-core/src/reflex.rs or the hold-out rate",
        "Change to reflex modes or defaults, the confirm veto words, hand-off cue or judge veto",
        "A product rename of xcb or ALGAL",
      ],
    },
  }),
  post({
    slug: "how-xcb-uses-wordcell",
    title: "How xcb uses Wordcell to give agents your notes",
    dek: "xcb project workers can search one Wordcell vault of your Markdown notes, and every hit names the note it came from.",
    eyebrow: "Integration",
    published: "2026-09-24",
    keywords: ["xcb", "Wordcell", "agent memory", "Markdown", "citations", "coding agents"],
    relation: "runtime:xcb:kb:searches-project-notes-with",
    statusInBody: true,
    sources: [
      { title: "xcb Wordcell module: pinning, exact search flags, note saving and its retry", href: xcb("crates/xcb-runtime/src/wordcell.rs"), checkedOn },
      { title: "xcb memory CLI: configure, status, search, promote", href: xcb("crates/xcb-cli/src/habitat.rs"), checkedOn },
      { title: "Worker tool definitions for xcb_memory_search and xcb_memory_recent", href: xcb("crates/xcb-runtime/src/broker.rs"), checkedOn },
      { title: "Project memory binding, search, and saving", href: xcb("crates/xcb-runtime/src/managed_project.rs"), checkedOn },
      { title: "Recent working memory for fresh workers, kept separate from Wordcell", href: xcb("crates/xcb-runtime/src/managed_habitat.rs"), checkedOn },
      { title: "Persistent project agents: working memory and Wordcell", href: xcb("docs/project-agents.md"), checkedOn },
      { title: "Wordcell README: exact search reads current Markdown with no model or network request", href: wordcell("README.md"), checkedOn },
      { title: "Wordcell agent memory: search modes and opt-in Git history", href: wordcell("docs/agent-memory.md"), checkedOn },
    ],
    admission: {
      lifecycle: "indexable",
      readerJob: "Decide whether to let xcb agents search a Markdown notes vault, and learn how to bind it, what workers can do with it, and how a note gets saved back.",
      nonObviousAnswer: "Nothing from the vault reaches a worker's prompt unless the worker calls xcb_memory_search, which runs Wordcell's exact mode against a hash-pinned binary and vault identity; workers have no write tool, and a save is a separate xcb memory promote step whose note name is a hash, so a retry cannot duplicate it.",
      originalContribution: "Pinning, the cleared environment, the dash-query refusal, hashed note names, and exit-code behavior read from wordcell.rs, habitat.rs, broker.rs, and managed_project.rs.",
      hostFit: "The registered runtime:xcb:kb:searches-project-notes-with relation carries the detail sentence this post explains, on the consumer's host.",
      nearestUrls: [
        { url: "https://github.com/hraness/xcb/blob/main/docs/project-agents.md", distinction: "The project-agent reference lists the commands; the post explains what a worker can and cannot do with the vault." },
      ],
      observations: [
        "Fresh workers receive recent task summaries automatically, but xcb labels them as its own working memory, separate from Wordcell notes, which reach a worker only through an explicit search.",
        "Only the memory tools are read-only; a worker's ordinary file tools could reach a vault placed inside the workspace, so the separation holds only when the vault lives outside it.",
      ],
      scores: { readerUtility: 2, originalEvidence: 2, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 1 },
      review: editorialReview,
      reassessOn: "2026-11-05",
      harmIfWrong: "A reader could place a vault inside the workspace believing workers cannot write to it.",
      refreshTriggers: [
        "The relation runtime:xcb:kb:searches-project-notes-with registering, changing its detail sentence, or being removed",
        "A change in wordcell.rs to the search flags, limits, pinning or note naming",
        "A change to the xcb_memory_search or xcb_memory_recent tool schema, or a new memory write tool for workers",
        "A Wordcell change to exact search or no-clobber note creation",
        "A new xcb release recorded in site/published-release.json",
      ],
    },
  }),
];

/** Posts whose body is Markdown in `content/blog/<slug>.md`. */
export const markdownBlogPosts: readonly BlogPost[] = blogPosts.filter((entry) => entry.format !== "beats");

export function findBlogPost(slug: string): BlogPost | undefined {
  return blogPosts.find((entry) => entry.slug === slug);
}

/** Posts that may appear in the index, sitemap, feed, and llms.txt, newest first. */
export const indexableBlogPosts: readonly BlogPost[] = blogPosts
  .filter((entry) => entry.admission.lifecycle === "indexable")
  .toSorted((left, right) => right.published.localeCompare(left.published));

export function blogPostPath(entry: Pick<BlogPost, "slug">): `/blog/${string}` {
  return `/blog/${entry.slug}`;
}

/** Midnight UTC on the post's date, the timestamp form feeds and JSON-LD expect. */
export function blogTimestamp(date: ArticleIsoDate): string {
  return `${date}T00:00:00.000Z`;
}
