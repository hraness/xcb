import type {
  ArticleAdmission,
  ArticleAuthor,
  ArticleIsoDate,
  ArticleSourceItem,
} from "@hraness/design-kit";

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
export const blogTitle = "xcb blog";
export const blogDescription = "How xcb routes coding tasks across your Claude, Codex, and Devin subscriptions, and how it uses other Hraness tools to do it.";

export const blogAuthor: ArticleAuthor = { kind: "organization", name: "Hraness" };

const reviewer = "Claude Opus 5.5 (claude-opus-5-5) editorial review";
const reviewedOn: ArticleIsoDate = "2026-09-24";
const checkedOn: ArticleIsoDate = "2026-09-24";

/** Sources pinned to the commits the fact check read. */
const xcb = (path: string) => `https://github.com/hraness/xcb/blob/6437bcb/${path}`;
const algalAt = (rev: string, path: string) => `https://github.com/hraness/algal/blob/${rev}/${path}`;
const gobstopperAt = (rev: string, path: string) => `https://github.com/hraness/gobstopper/blob/${rev}/${path}`;
const wordcell = (path: string) => `https://github.com/hraness/wordcell/blob/7b6cb5e/${path}`;
const designKitPortfolio = "https://github.com/hraness/design-kit/blob/v0.17.0/src/portfolio.generated.json";

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
  sources: readonly ArticleSourceItem[];
  admission: ArticleAdmission;
}>;

function sourceRecords(sources: readonly ArticleSourceItem[]) {
  return sources.map(({ title, href, checkedOn: checked }) => ({ title, url: href, checkedOn: checked }));
}

function post(
  entry: Omit<BlogPost, "admission"> & Readonly<{
    admission: Omit<ArticleAdmission, "href" | "sources" | "drafting" | "review" | "humanReview" | "owner">;
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
      review: { reviewer, reviewerType: "ai", reviewedOn },
      humanReview: null,
    },
  };
}

export const blogPosts: readonly BlogPost[] = [
  post({
    slug: "introducing-xcb",
    title: "Introducing xcb",
    dek: "xcb sends each coding task to one of your Claude, Codex, or Devin accounts that is signed in, idle, and not at a known quota limit.",
    eyebrow: "Release",
    published: "2026-09-24",
    keywords: ["xcb", "coding agents", "routing", "Claude Code", "Codex", "Devin"],
    relation: "all",
    statusInBody: false,
    sources: [
      { title: "xcb README: lead, readiness table, managed conversations, optional behavior", href: xcb("README.md"), checkedOn },
      { title: "Reflexes reference: defaults, certification floors, measured history", href: xcb("docs/reflexes.md"), checkedOn },
      { title: "Published release record", href: xcb("site/published-release.json"), checkedOn },
      { title: "Context trimming with Gobstopper's elision policy", href: xcb("crates/xcb-runtime/src/context.rs"), checkedOn },
      { title: "Pinned ALGAL planners and resumable controllers", href: xcb("crates/xcb-runtime/src/managed_program.rs"), checkedOn },
      { title: "Registered relation: Gobstopper compacts sessions for xcb", href: designKitPortfolio, checkedOn },
      { title: "xcb releases", href: "https://github.com/hraness/xcb/releases", checkedOn },
    ],
    admission: {
      lifecycle: "indexable",
      readerJob: "Decide whether xcb fits a workflow that already spans more than one of Claude Code, Codex, and Devin, and start a first managed conversation.",
      nonObviousAnswer: "xcb locks the chosen account until the provider process exits, trims old tool output for Claude and Codex (never the eight most recent), lets a reflex act only after your own replies certify it (at least 30 turns, precision floors 0.75 and 0.85, about one in ten held out), and replays each task's local record offline without vouching for the provider.",
      originalContribution: "One account of routing, context trimming, reflexes, and replayable history, with commands and limits taken from xcb source at 6437bcb rather than from the home page.",
      hostFit: "The product's own introduction on its own host.",
      nearestUrls: [
        { url: "/docs/getting-started", distinction: "The guide gives setup steps for each provider; the post explains why xcb exists and who should use something else." },
        { url: "/", distinction: "The home page lists features; the post shows one first conversation and states the limits beside it." },
      ],
      observations: [
        "Two separate protections keep recent work in the prompt: Gobstopper's elide rule skips the eight newest tool outputs, and xcb separately refuses to touch the last eight messages of any role (context.rs).",
        "The --plan flag on xcb accounts add is a display label that checks nothing, so routing depends on xcb's own sign-in and quota observations rather than the plan name.",
      ],
      scores: { readerUtility: 2, originalEvidence: 1, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 1 },
      reassessOn: "2026-11-05",
      harmIfWrong: "A reader could install xcb expecting a daily-driver replacement, or trust a reflex or history check further than the source supports.",
      refreshTriggers: [
        "xcb release tag bump (site/published-release.json), including publication of v0.8.1 with the Claude Code 2.1.281 fix",
        "Change to reflex defaults, the 30-turn minimum, the 0.75/0.85 floors, or the hold-out rate",
        "Change to Gobstopper elision defaults, the recent-output count, or the optional judge",
        "Change to the README readiness table or supported provider builds",
        "Change to ALGAL managed-program limits (--managed-calls) or tasks verify semantics",
        "Change to a registered relation detail for xcb, or a product rename",
      ],
    },
  }),
  post({
    slug: "how-xcb-uses-gobstopper",
    title: "How xcb uses Gobstopper to trim stale tool output",
    dek: "Once a Claude Code or Codex session passes a size threshold, xcb uses Gobstopper to replace old tool output in the prompt with a short marker and keeps the original locally.",
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
    dek: "xcb's continuation reflexes run as small ALGAL programs and, by default, act only after your own replies certify them, with about one turn in ten still left for you to answer.",
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
      ],
      scores: { readerUtility: 2, originalEvidence: 1, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 1 },
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
