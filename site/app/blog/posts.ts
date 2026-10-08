import type {
  ArticleAdmission,
  ArticleAuthor,
  ArticleHumanReview,
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
export const blogDescription = "How xcb routes coding tasks across your Claude and Codex subscriptions, and how it uses other Hraness tools to do it.";

export const blogAuthor: ArticleAuthor = { kind: "organization", name: "Hraness" };


/** Independent review of all five revised Markdown bodies on October 1. */
const revisionReview: ArticleReview = {
  reviewer: "Codex",
  reviewerType: "ai",
  reviewedOn: "2026-10-01",
};
const revisionCheckedOn: ArticleIsoDate = "2026-10-01";
const revisionAt = (path: string) => `https://github.com/hraness/xcb/blob/1e64357a40fb9b0cb167e9803bc572675100d469/${path}`;

/** Sources pinned to the commits the fact check read. */
const xcbAt = (rev: string, path: string) => `https://github.com/hraness/xcb/blob/${rev}/${path}`;

/**
 * Ben Guo's review in the October 4 editorial pass, recorded both as the
 * review of the posts it revised and as the human review of every post.
 * Earlier records are archived in docs/editorial.
 */
const editorReview = {
  reviewer: "Ben Guo",
  reviewerType: "human-editor",
  reviewedOn: "2026-10-04",
} as const satisfies ArticleHumanReview;
const editorCheckedOn: ArticleIsoDate = "2026-10-04";
const editorReassessOn: ArticleIsoDate = "2026-11-15";
/** Sources the October 4 pass checked, pinned to the v0.18.0 release tag. */
const releaseAt = (path: string) => xcbAt("v0.18.0", path);

export type BlogPost = Readonly<{
  slug: string;
  title: string;
  dek: string;
  eyebrow: string;
  published: ArticleIsoDate;
  updated?: ArticleIsoDate;
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
    admission: Omit<ArticleAdmission, "href" | "sources" | "drafting" | "humanReview" | "owner"> & Readonly<{
      /** A person's review on record. Never an AI review; AI review stays in `review`. */
      humanReview?: ArticleHumanReview | null;
    }>;
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
      humanReview: entry.admission.humanReview ?? null,
    },
  };
}

export const blogPosts: readonly BlogPost[] = [
  post({
    slug: "how-xcb-uses-aicharts",
    title: "How xcb uses aicharts to show your token use",
    dek: "The xcb installer adds aicharts, which keeps a daily record of your agents' token use on your computer.",
    eyebrow: "Integration",
    published: "2026-10-04",
    keywords: ["xcb", "aicharts", "token usage", "coding agents", "Claude Code", "Codex", "MCP"],
    relation: "runtime:xcb:aicharts:installs",
    statusInBody: false,
    sources: [
      { title: "xcb usage: forwarded history commands and the connect registration", href: releaseAt("crates/xcb-cli/src/usage.rs"), checkedOn: editorCheckedOn },
      { title: "Installer: pinned aicharts digests, signature check and first-install history", href: releaseAt("scripts/install.sh"), checkedOn: editorCheckedOn },
      { title: "Usage history tools: what connect registers and when to renew it", href: releaseAt("docs/tools.md"), checkedOn: editorCheckedOn },
      { title: "xcb doctor usage-history line", href: releaseAt("crates/xcb-cli/src/doctor.rs"), checkedOn: editorCheckedOn },
      { title: "Provider runs: private Claude Code and Codex profiles, Claude without saved sessions", href: releaseAt("crates/xcb-runtime/src/runner.rs"), checkedOn: editorCheckedOn },
      { title: "Usage extensions and local aicharts exports", href: releaseAt("crates/xcb-runtime/src/exports.rs"), checkedOn: editorCheckedOn },
      { title: "aicharts usage history and agent queries", href: "https://github.com/hraness/aicharts/blob/main/docs/usage-history.md", checkedOn: editorCheckedOn },
      { title: "aicharts CLI 0.3.1 release", href: "https://github.com/hraness/aicharts/releases/tag/cli-v0.3.1", checkedOn: editorCheckedOn },
    ],
    admission: {
      lifecycle: "indexable",
      readerJob: "Find out what the xcb installer adds for usage history, how to read it, and how to let routed tasks query it.",
      nonObviousAnswer: "xcb's quota view and aicharts' record answer different questions, the record covers the agents' own session folders but not the runs xcb starts, and the tools xcb usage connect gives tasks are pinned to one aicharts build, so they need reconnecting after an aicharts update; the installer and xcb doctor handle that.",
      originalContribution: "The installer's checks and defaults, the five forwarded history commands, the pinned host tool registration and its renewal path, and why xcb's own runs stay out of the record, read from xcb's usage.rs, install.sh, doctor.rs, runner.rs, exports.rs and docs/tools.md at v0.18.0.",
      hostFit: "A How xcb uses aicharts post on xcb's host for the registered relation runtime:xcb:aicharts:installs, expanding its detail sentence.",
      nearestUrls: [
        { url: "/docs/reference", distinction: "The reference lists the commands; the post explains where the numbers come from and what connect registers." },
        { url: "https://aicharts.io/usage", distinction: "aicharts' page covers the collector and its dashboard; this post covers what xcb installs and exposes to tasks." },
      ],
      observations: [
        "The installer verifies the aicharts archive against digests pinned in scripts/install.sh, not a checksum file fetched alongside it.",
        "Host tool servers get a private home, so the registration names aicharts' record folder through AICHARTS_HOME.",
        "xcb starts Claude Code with --no-session-persistence and a private CLAUDE_CONFIG_DIR, and Codex with a private CODEX_HOME, so aicharts' daily record does not see xcb's own runs.",
      ],
      scores: { readerUtility: 2, originalEvidence: 1, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 2 },
      review: editorReview,
      humanReview: editorReview,
      reassessOn: editorReassessOn,
      harmIfWrong: "A reader could confuse quota left with spend, expect usage data to leave the computer, expect xcb's own runs in the daily record, or miss that the task tools need reconnecting after an aicharts update.",
      refreshTriggers: [
        "Change to the relation runtime:xcb:aicharts:installs",
        "Change to the pinned aicharts version or digests in scripts/install.sh",
        "Change to the forwarded commands or the registration in crates/xcb-cli/src/usage.rs",
        "Change to aicharts' collection schedule or MCP tools",
        "A change to where xcb's provider runs keep session files, or aicharts reading xcb's exports",
        "xcb release tag bump",
        "Rename of xcb or aicharts",
      ],
    },
  }),
  post({
    slug: launchPostSlug,
    title: "Introducing Excalibur: operate your AI subscriptions",
    dek: "Log in with all your Codex and Claude accounts, tell your agent to use xcb, and let long jobs scale to the usage left on each account.",
    eyebrow: "Launch",
    updated: "2026-10-04",
    published: "2026-09-29",
    keywords: ["xcb", "Excalibur", "Claude Code", "Codex", "multiple AI subscriptions", "usage limits", "coding agents"],
    relation: "all",
    statusInBody: false,
    format: "beats",
    sources: [
      { title: "xcb README: providers, how routing works, everyday commands, limits", href: releaseAt("README.md"), checkedOn: editorCheckedOn },
      { title: "Quota routing: fresh usage within five minutes, Claude and Codex windows, failover on a reported limit", href: releaseAt("docs/quota-routing.md"), checkedOn: editorCheckedOn },
      { title: "Agent and SDK interface: xcb run, the JSON route, and task projections", href: releaseAt("docs/terminal.md"), checkedOn: editorCheckedOn },
      { title: "Route contract: the JSON request and result", href: releaseAt("docs/route.md"), checkedOn: editorCheckedOn },
      { title: "Launch facts", href: releaseAt("site/app/launch/facts.ts"), checkedOn: editorCheckedOn },
      { title: "Registered host tools and browser access", href: releaseAt("docs/tools.md"), checkedOn: editorCheckedOn },
      { title: "Published release record", href: xcbAt("c0ff3abb21a04df49dc17f15c8ddf61e8bf3d7b0", "site/published-release.json"), checkedOn: editorCheckedOn },
    ],
    admission: {
      lifecycle: "indexable",
      readerJob: "Decide in a minute whether xcb is for you when you pay for more than one AI coding plan, and share the one piece that makes the case.",
      nonObviousAnswer: "xcb does not add capacity; it spends the quota you already have in the right order, favoring unused quota close to a reset, and moves a task to another account when a provider reports a limit mid-task.",
      originalContribution: "Nine standalone claims, each checked against xcb's README and docs at v0.18.0 and paired with an illustration drawn from the CLI's own output shapes, which the social posts are cut from without rewording.",
      hostFit: "The product's own launch post on its own host, with the long introduction at /blog/introducing-excalibur as its technical companion.",
      nearestUrls: [
        { url: "/blog/introducing-excalibur", distinction: "The introduction follows one task through sign-in, sandbox, and recorded outcome in long form; this post gives the same product as short standalone claims for a first look and for sharing." },
        { url: "/", distinction: "The home page answers setup questions and installs; this post makes the case one claim at a time." },
      ],
      observations: [
        "Every number in the post comes from app/launch/facts.ts, and tests read the README and docs to check each one.",
        "The illustrations reuse the exact line shapes `xcb accounts`, `xcb tasks`, `xcb attention`, and the managed runtime's start and limit notices print, and a test fails if a shape drifts from the CLI source.",
      ],
      scores: { readerUtility: 2, originalEvidence: 1, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 2 },
      review: editorReview,
      humanReview: editorReview,
      reassessOn: editorReassessOn,
      harmIfWrong: "A reader could expect xcb to add usage, to keep provider plugins inside its runs, to open an interactive terminal, or to move every task on any limit, and pay for a plan they would not use.",
      refreshTriggers: [
        "xcb release tag bump (site/published-release.json)",
        "A change to quota routing freshness, the windows xcb reads, or failover on a reported limit",
        "A change to xcb run, xcb backlog, or the background supervisor that runs managed tasks",
        "Remote commands returning, for example over Valhalla",
        "A change to the output shapes of xcb accounts, tasks, attention, or the managed runtime notices",
      ],
    },
  }),
  post({
    slug: "introducing-excalibur",
    title: "Introducing Excalibur",
    dek: "xcb, short for Excalibur, sends each coding task to one of your Claude or Codex accounts that is signed in, idle, and not at a known usage limit.",
    eyebrow: "Release",
    updated: "2026-10-04",
    published: "2026-09-27",
    keywords: ["xcb", "Excalibur", "coding agents", "routing", "Claude Code", "Codex"],
    relation: "all",
    statusInBody: true,
    sources: [
      { title: "Current provider support, task routing, and the headless commands", href: releaseAt("README.md"), checkedOn: editorCheckedOn },
      { title: "Agent and SDK interface: xcb run, the JSON route, and task projections", href: releaseAt("docs/terminal.md"), checkedOn: editorCheckedOn },
      { title: "Claude and Codex usage, cooldowns, and account constraints", href: releaseAt("docs/quota-routing.md"), checkedOn: editorCheckedOn },
      { title: "Registered host tools and browser access", href: releaseAt("docs/tools.md"), checkedOn: editorCheckedOn },
      { title: "Installer: xcb and the pinned aicharts build", href: releaseAt("scripts/install.sh"), checkedOn: editorCheckedOn },
    ],
    admission: {
      lifecycle: "indexable",
      readerJob: "Decide whether xcb is worth installing today when you pay for more than one of Claude and Codex or build agent tooling, and know the first commands to run.",
      nonObviousAnswer: "xcb runs provider tools under your sign-in, holds an account until the provider process exits, and sandboxes the run. The route command picks the account and model; an SDK host names both. Registered host tools run separately with their own permissions.",
      originalContribution: "One account of how xcb runs a task, the headless run, backlog, route, and SDK uses, and its account, sandbox, host-tool, and usage limits, revised against the v0.18.0 source and linked to detailed guides.",
      hostFit: "The product’s introduction on its own host; the retired Introducing xcb URL redirects here.",
      nearestUrls: [
        { url: "/docs/getting-started", distinction: "The guide gives install and sign-in steps for each provider; the post explains what xcb does, who it suits, and its limits, and ends with the shortest start." },
        { url: "/", distinction: "The home page lists features and answers setup questions; the post follows one task from sign-in to its recorded outcome and keeps the limits in one place." },
        { url: "/compare", distinction: "The comparison pages weigh xcb against each tool in detail; the post gives a first-time reader the short version and links there." },
      ],
      observations: [
        "Native provider shells remain disabled while registered host tools can offer additional capabilities outside that sandbox.",
        "Fresh Claude and Codex usage reports affect account preference only within the task’s quality requirements and explicit account, model, or provider constraints.",
      ],
      scores: { readerUtility: 2, originalEvidence: 1, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 2 },
      review: editorReview,
      humanReview: editorReview,
      reassessOn: editorReassessOn,
      harmIfWrong: "A reader could install xcb expecting Linux coding sessions, the newest Codex build, or an interactive terminal to work today, run several accounts believing xcb raises usage limits, or enable a hook believing it runs inside xcb's sandbox.",
      refreshTriggers: [
        "xcb release tag bump (site/published-release.json)",
        "A change to xcb run, xcb backlog, or the background supervisor that runs backlog tasks",
        "A change to which providers have a confirmed coding session, or to the supported Codex builds: update the Limits paragraph",
        "A change to the supported Codex builds (qualified-builds.json) or the Claude Code version floor, or a coding session confirmed for Claude on Linux or on the current Codex build",
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
    updated: "2026-10-01",
    published: "2026-09-24",
    keywords: ["xcb", "Gobstopper", "context management", "coding agents", "Claude Code", "Codex"],
    relation: "runtime:gobstopper:xcb:compacts-sessions-for",
    statusInBody: false,
    sources: [
      { title: "Current provider selection, saved history, and judge input limits", href: revisionAt("crates/xcb-runtime/src/context.rs"), checkedOn: revisionCheckedOn },
      { title: "Judge failure and deterministic elision fallback", href: revisionAt("crates/xcb-runtime/src/runner.rs"), checkedOn: revisionCheckedOn },
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
        "All supported provider sessions use this elision policy.",
      ],
      scores: { readerUtility: 2, originalEvidence: 1, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 2 },
      review: revisionReview,
      humanReview: editorReview,
      reassessOn: "2026-11-12",
      harmIfWrong: "A reader could expect elided output to be gone for good, or expect provider sessions to be trimmed, and size their sessions on a wrong assumption.",
      refreshTriggers: [
        "Change to the relation detail runtime:gobstopper:xcb:compacts-sessions-for",
        "gobstopper-core rev bump in crates/xcb-runtime/Cargo.toml",
        "Change to ContextPolicy defaults or validation in config.rs, or to selection rules, marker text or judge rules in context.rs",
        "Change to the elision notice or judge fallback in runner.rs",
        "Provider sessions changing their compaction policy",
        "xcb release tag bump",
        "Rename of xcb or Gobstopper",
      ],
    },
  }),
  post({
    slug: "replayable-task-history",
    title: "How to check an xcb task history offline",
    dek: "xcb tasks verify checks a task’s local history for inconsistent records and missing steps. Understand what a passing check can and cannot prove.",
    eyebrow: "Technique",
    updated: "2026-10-01",
    published: "2026-09-24",
    keywords: ["xcb", "replay", "verification", "coding agents", "ALGAL"],
    relation: null,
    statusInBody: false,
    sources: [
      { title: "Current verifier and consistency limits", href: revisionAt("crates/xcb-runtime/src/managed.rs"), checkedOn: revisionCheckedOn },
      { title: "Additive migration preserves old task records", href: revisionAt("crates/xcb-runtime/src/managed_habitat_tests.rs"), checkedOn: revisionCheckedOn },
      { title: "Task retention and live-task exemptions", href: revisionAt("crates/xcb-runtime/src/managed_recovery_tests.rs"), checkedOn: revisionCheckedOn },
    ],
    admission: {
      lifecycle: "indexable",
      readerJob: "Check an xcb managed task’s local history for inconsistencies and understand the limits of a passing result.",
      nonObviousAnswer: "Run `xcb tasks verify` against a copy of the state directory: it replays recorded revisions through ALGAL offline and fails on inconsistent or missing entries, but it is a consistency check with no signature, so it cannot catch a fully rebuilt history or judge the work itself.",
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
      scores: { readerUtility: 2, originalEvidence: 2, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 2 },
      review: revisionReview,
      humanReview: editorReview,
      reassessOn: "2026-11-12",
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
    title: "How xcb uses ALGAL to learn when to continue",
    dek: "Your replies train xcb’s continuation decisions. Small ALGAL programs compute those decisions, while host permission checks still control when work can continue.",
    eyebrow: "Integration",
    updated: "2026-10-01",
    published: "2026-09-24",
    keywords: ["xcb", "ALGAL", "reflexes", "coding agents", "routing", "replay"],
    relation: "runtime:xcb:algal:replays-task-history-with",
    statusInBody: false,
    sources: [
      { title: "Operator-only certification, held-out replies, and statistical limitations", href: revisionAt("docs/reflexes.md"), checkedOn: revisionCheckedOn },
      { title: "Effect-free reflex programs", href: revisionAt("crates/xcb-runtime/src/reflex.rs"), checkedOn: revisionCheckedOn },
      { title: "Automatic confirmation cue checks", href: revisionAt("crates/xcb-core/src/reflex.rs"), checkedOn: revisionCheckedOn },
      { title: "Host continuation checks outside replaceable programs", href: revisionAt("crates/xcb-runtime/src/managed.rs"), checkedOn: revisionCheckedOn },
    ],
    admission: {
      lifecycle: "indexable",
      readerJob: "Decide whether to let xcb's reflexes continue stopped turns and answer go-ahead requests for you, and know what stops them from acting on a risky turn.",
      nonObviousAnswer: "ALGAL programs compute reflex decisions without model calls or side effects. Continuation in auto mode depends on replayed operator labels; labels from automatic actions train the model but cannot certify it. Estimated thresholds and cue-based vetoes limit automatic action without guaranteeing correctness.",
      originalContribution: "The separation between learned parameters, replaceable programs, and host permission checks, including operator-only certification and cue-based veto limits.",
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
      scores: { readerUtility: 2, originalEvidence: 1, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 2 },
      review: revisionReview,
      humanReview: editorReview,
      reassessOn: "2026-11-12",
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
    updated: "2026-10-01",
    published: "2026-09-24",
    keywords: ["xcb", "Wordcell", "agent memory", "Markdown", "citations", "coding agents"],
    relation: "runtime:xcb:kb:searches-project-notes-with",
    statusInBody: false,
    sources: [
      { title: "Working memory and Wordcell notes", href: revisionAt("docs/project-agents.md"), checkedOn: revisionCheckedOn },
      { title: "Launcher and vault identity, search flags, and explicit note promotion", href: revisionAt("crates/xcb-runtime/src/wordcell.rs"), checkedOn: revisionCheckedOn },
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
      scores: { readerUtility: 2, originalEvidence: 1, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 2 },
      review: revisionReview,
      humanReview: editorReview,
      reassessOn: "2026-11-12",
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
