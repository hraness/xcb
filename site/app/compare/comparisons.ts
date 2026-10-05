import type { MarketingComparisonRow } from "@hraness/design-kit/react/server";
import type { ArticleIsoDate } from "@hraness/design-kit";

/**
 * The words on every "xcb vs <tool>" page. `[slug]/page.tsx` renders each
 * record through `comparison-page.tsx`, and the hub lists them, so a new
 * comparison is one record here plus a line in `public/sitemap.xml`.
 *
 * Text fields accept two inline marks and nothing else: `code` in backticks
 * and [a link](href). Links that leave xcb.sh get ↗ from the renderer. Table
 * cells take no links. Describe the other product from its own dated
 * documentation, quote providers in their own words, and keep each xcb limit
 * beside the claim it limits (STYLE.md, AGENTS.md "Public copy").
 */

export interface ComparisonSource {
  readonly label: string;
  readonly href: string;
  /** The day the source was last read for this page. */
  readonly checkedOn: ArticleIsoDate;
}

export interface ComparisonRow {
  /** Four words or fewer. */
  readonly aspect: string;
  /** 120 characters or fewer, no links. */
  readonly tool: string;
  /** 120 characters or fewer, no links. */
  readonly xcb: string;
}

export interface ComparisonSection {
  readonly id: string;
  readonly title: string;
  readonly paragraphs: readonly string[];
}

export interface Comparison {
  readonly slug: string;
  /** The other product's name as its makers write it. */
  readonly tool: string;
  /** The page's h1 when "xcb vs {tool}" would mislead. */
  readonly heading?: string;
  /** The document title: 70 characters or fewer, naming xcb once. */
  readonly title: string;
  /** Meta description: 110 to 160 characters, unique on the site. */
  readonly description: string;
  /** The verdict: what each product is and when to pick which, in one or two sentences. */
  readonly lead: string;
  /** The day the page last materially changed; individual sources retain their own check dates. */
  readonly updated: ArticleIsoDate;
  readonly picks: Readonly<{ tool: readonly string[]; xcb: readonly string[] }>;
  /** Compact, qualified overview; detailed source-backed rows follow in a disclosure. */
  readonly glance: readonly MarketingComparisonRow[];
  /** Five to eight rows. */
  readonly rows: readonly ComparisonRow[];
  /** Shown under the table. */
  readonly note?: string;
  readonly sections?: readonly ComparisonSection[];
  readonly sources: readonly ComparisonSource[];
}

/** Table cells that state the same xcb fact on several pages. */
export const xcbFacts = {
  platforms: "Mac ARM64, Linux x86_64/ARM64, Windows x86_64; Windows provider work needs WSL2; Codex needs macOS",
  sandbox: "Every provider run is sandboxed; commands run in an offline Linux VM on macOS ARM64",
  route: "`xcb --json route` takes one task and returns the result, the account and model used, and how the run ended",
} as const;

const xcbPlatforms = {
  status: "partial",
  label: "Mac, Linux, Windows",
  detail: "Mac ARM64; Linux x86_64/ARM64; Windows x86_64. Claude on Linux needs sandbox setup. Windows provider work uses WSL2; Codex needs Mac.",
} as const;

const checkedOn: ArticleIsoDate = "2026-09-27";

const anthropicLegal = "https://code.claude.com/docs/en/legal-and-compliance";

/** Anthropic's own sentence about signing in to Claude Code, quoted wherever xcb's Claude access comes up. */
const claudeCodeSignIn = `[Anthropic’s Claude Code legal page](${anthropicLegal}) says its credential rules don’t “prevent an end user from signing in to the unmodified Claude Code binary with their own Claude subscription”.`;

const xcbClaudeSignIn = "xcb runs the unmodified Claude Code binary, signed in through Claude Code’s own `claude setup-token` flow.";

export const comparisons: readonly Comparison[] = [
  {
    slug: "herdr",
    glance: [
      { label: "Focus", values: ["Terminal workspaces", "Task and account routing"] },
      { label: "Agents", values: ["22 agent CLIs", "Claude Code and Codex"] },
      { label: "After disconnect", values: [{ status: "yes", label: "Background server" }, { status: "yes", label: "Task supervisor" }] },
      { label: "Run isolation", values: [{ status: "depends", label: "Agent permissions", detail: "Plugin code is not sandboxed." }, { status: "yes", label: "Sandboxed provider runs", detail: "Commands use an offline VM on Mac ARM64." }] },
      { label: "Platforms", values: ["Mac, Linux, Windows", xcbPlatforms] },
      { label: "License", values: ["Apache 2.0 · free", "MIT · free"] },
    ],
    tool: "herdr",
    title: "xcb vs herdr: where agents live, and which account runs a task",
    description: "herdr keeps coding agents running in terminals you can reattach to from any machine. xcb picks the account for each task and can run in a herdr pane.",
    lead: "herdr keeps your coding agents running in real terminals that you can reattach to from any machine, and shows which one needs you. xcb picks which of your Claude and Codex accounts runs each task and holds that account until the run ends, and it can run in a herdr pane, so you can use both.",
    updated: checkedOn,
    picks: {
      tool: [
        "You run several agent CLIs and want each in a real terminal that keeps running after you disconnect.",
        "You want to see which agent is working, blocked, or done across all your projects.",
        "You work across machines over SSH, or on Windows.",
        "You want plugins, or agents that open and drive panes through a socket API.",
      ],
      xcb: [
        "You pay for more than one of Claude and Codex, or hold several accounts, and want each task sent to one that can take it now.",
        "You want each account to run one task at a time, held until that run ends.",
        "You want another agent to hand off a whole task with `xcb --json route` and get the result back.",
        "You want each provider run in a sandbox.",
      ],
    },
    rows: [
      { aspect: "What it manages", tool: "Terminals: sessions, workspaces, tabs, and panes, each running a real process", xcb: "Tasks: which account and model runs each one, and how the run ended" },
      { aspect: "Agents", tool: "22 detected agent CLIs, including Claude Code, Codex, pi, and OpenCode", xcb: "Claude Code and Codex, run by xcb instead of through their own interfaces" },
      { aspect: "Accounts and limits", tool: "Each agent keeps its own sign-in; herdr runs it unchanged", xcb: "Each task goes to an idle account that isn’t at a known limit, one task per account" },
      { aspect: "After you disconnect", tool: "Agents keep running in herdr’s background server", xcb: "Tasks keep running under xcb’s background supervisor" },
      { aspect: "For other agents", tool: "A CLI and socket API that agents use to split panes, start and prompt other agents, and wait on them", xcb: xcbFacts.route },
      { aspect: "Sandboxing", tool: "Agents keep their own permission settings; herdr doesn’t sandbox plugin code", xcb: xcbFacts.sandbox },
      { aspect: "Platforms", tool: "macOS, Linux, and Windows", xcb: xcbFacts.platforms },
      { aspect: "License and price", tool: "Apache 2.0, free", xcb: "MIT, free" },
    ],
    sections: [
      {
        id: "together",
        title: "Using both",
        paragraphs: [
          "xcb runs in a herdr pane like any other terminal program. herdr keeps that terminal open and reachable from your other machines, and xcb’s own supervisor keeps tasks running either way.",
          "herdr shows working, blocked, and done states for the agents it recognizes and for tools that report their state over its socket API. xcb doesn’t report its state to herdr, so its pane appears as an ordinary terminal.",
        ],
      },
    ],
    sources: [
      { label: "herdr home page", href: "https://herdr.dev/", checkedOn },
      { label: "herdr agent support", href: "https://herdr.dev/docs/agents/", checkedOn },
      { label: "herdr plugins, trust and security", href: "https://herdr.dev/docs/plugins/", checkedOn },
      { label: "herdr socket API", href: "https://herdr.dev/docs/socket-api/", checkedOn },
      { label: "xcb: Route tasks", href: "/docs/route", checkedOn },
    ],
  },
  {
    slug: "pi",
    glance: [
      { label: "Focus", values: ["Extensible coding agent", "Router around provider agents"] },
      { label: "Model access", values: ["15+ providers; keys or OAuth", "Claude and Codex plans"] },
      { label: "Extensions", values: ["Tools, prompts, and interface", "Panes, hooks, and reflexes"] },
      { label: "Run isolation", values: [{ status: "optional", label: "Bring your own container" }, { status: "yes", label: "Sandboxed provider runs", detail: "Commands use an offline VM on Mac ARM64." }] },
      { label: "Platforms", values: ["Mac, Linux, Windows", xcbPlatforms] },
      { label: "License", values: ["MIT", "MIT"] },
    ],
    tool: "pi",
    heading: "xcb and pi",
    title: "xcb and pi: two coding harnesses you can reshape",
    description: "pi is a minimal coding agent you rebuild with TypeScript extensions. xcb routes tasks to your own subscriptions and keeps every provider run in a sandbox.",
    lead: "pi is a minimal coding agent that you reshape with TypeScript extensions, and it calls 15+ model providers directly. xcb sits above the providers’ own agents and routes each task to one of your subscriptions; you can reshape its terminal and routing, and nothing you add widens what a provider run may do.",
    updated: checkedOn,
    picks: {
      tool: [
        "You want to change the agent itself: its tools, prompts, context handling, and interface.",
        "You want one agent across 15+ providers, your own API keys, or local models.",
        "You want the agent to work directly on your machine, with your shell, Git, and network.",
        "You want to embed an agent with its SDK or drive it over RPC.",
      ],
      xcb: [
        "You pay for Claude or Codex and want each task to run through that provider’s own agent.",
        "You want each task sent to an account that can take it now, and held there until the run ends.",
        "You want to customize the harness and still have every provider run sandboxed with the same tools.",
      ],
    },
    rows: [
      { aspect: "What it is", tool: "A coding agent with a minimal core that you extend", xcb: "A router and terminal around the providers’ own agents" },
      { aspect: "Model access", tool: "15+ providers through API keys or OAuth, including Claude Pro/Max and ChatGPT plans", xcb: "Your Claude and Codex subscriptions, through Claude Code and Codex" },
      { aspect: "What you change", tool: "Tools, commands, prompts, compaction, and the whole interface, through TypeScript extensions", xcb: "Panes, hooks, and reflex programs; none of them can add tools or permissions to a provider run" },
      { aspect: "Permissions", tool: "No permission prompts; run it in a container or build your own confirmation flow", xcb: xcbFacts.sandbox },
      { aspect: "Scripting", tool: "Print and JSON modes, RPC over stdin and stdout, and an SDK", xcb: xcbFacts.route },
      { aspect: "Learning", tool: "Ask pi to write an extension, then load it with `/reload`", xcb: "Two reflexes learn which model tier you want and when a task stopped early; you can roll them back" },
      { aspect: "Platforms", tool: "macOS, Linux, and Windows", xcb: xcbFacts.platforms },
      { aspect: "License", tool: "MIT", xcb: "MIT" },
    ],
    sections: [
      {
        id: "reshape",
        title: "What you can reshape",
        paragraphs: [
          "Both projects treat the harness as yours to change. In pi, the agent loop itself is yours: an extension can add tools, replace compaction, or redraw the interface, and pi can write the extension for you.",
          "xcb keeps the agent loop inside each provider’s own tool and lets you reshape what surrounds it. Panes describe what the terminal shows, and `/pane generate` writes one from a description. Hooks run programs you choose when a session or turn starts or ends. Reflex programs change how xcb picks a model tier or reads how a turn ended.",
          "None of these can give a provider run more tools, files, or network access. Panes are layout, not code. A hook stays off until you enable it, runs with a cleared environment, and won’t run again if its file changes. A replacement reflex program runs only if it has no side effects and calls no agents. A harness that tunes its own routing rules is in development, and the current build does not run self-modifying routing policies.",
        ],
      },
      {
        id: "claude",
        title: "Using a Claude subscription",
        paragraphs: [
          `pi can sign in to a Claude Pro or Max plan itself, or use an Anthropic API key, and calls Claude directly. ${xcbClaudeSignIn} [Anthropic’s Claude Code legal page](${anthropicLegal}) says OAuth sign-in with a Claude plan “is designed to support ordinary use of Claude Code and other native Anthropic applications.” The same page says its rules don’t “prevent an end user from signing in to the unmodified Claude Code binary with their own Claude subscription”.`,
          "For ChatGPT plans, OpenAI’s [Codex for Open Source page](https://developers.openai.com/community/codex-for-oss) says developers “should code in the tools they prefer, whether that’s Codex, OpenCode, Cline, pi, OpenClaw, or something else”.",
        ],
      },
    ],
    sources: [
      { label: "pi home page", href: "https://pi.dev/", checkedOn },
      { label: "pi AI library: OAuth providers", href: "https://github.com/earendil-works/pi/blob/main/packages/ai/README.md#oauth-providers", checkedOn },
      { label: "Anthropic: Claude Code legal and compliance", href: anthropicLegal, checkedOn },
      { label: "OpenAI: Codex for Open Source", href: "https://developers.openai.com/community/codex-for-oss", checkedOn },
      { label: "xcb: Make it yours", href: "/docs/customization", checkedOn },
      { label: "xcb: Learned routing and continuation", href: "/docs/reflexes", checkedOn },
    ],
  },
  {
    slug: "claude-code",
    glance: [
      { label: "Model access", values: ["Claude plan, API, or cloud", "Existing provider subscriptions"] },
      { label: "Accounts", values: ["One account per sign-in", "Route among available accounts"] },
      { label: "Sandboxing", values: [{ status: "optional", label: "OS shell sandbox" }, { status: "yes", label: "Whole-process sandbox" }] },
      { label: "Worker Git writes", values: [{ status: "yes", label: "Commit and push" }, { status: "no", label: "Read-only Git" }] },
      { label: "After closing", values: [{ status: "depends", label: "Cloud sessions only" }, { status: "yes", label: "Local task supervisor" }] },
      { label: "Platforms", values: ["Mac, Linux, WSL, Windows", xcbPlatforms] },
    ],
    tool: "Claude Code",
    heading: "xcb vs Claude Code and Codex",
    title: "xcb vs Claude Code and Codex: when to add a router",
    description: "Use Claude Code or Codex on its own when one subscription covers you. Add xcb when you pay for several and want each task sent to an account that can take it.",
    lead: "Use Claude Code or Codex on its own when one subscription covers your work and you want every native feature. Add xcb when you pay for more than one: it runs those same tools under your accounts, sends each task to one that can take it now, and keeps the work going after you close the terminal.",
    updated: "2026-10-04",
    picks: {
      tool: [
        "One Claude subscription covers your work.",
        "You want its plugins, skills, MCP servers, subagents, web search, or IDE and desktop apps.",
        "You want cloud sessions, scheduled routines, or Remote Control from your phone.",
        "You want the agent to commit, push, or run native macOS builds.",
      ],
      xcb: [
        "You pay for more than one of Claude and Codex, or hold more than one account, and want one place to send work.",
        "You want each task sent to an idle account that isn’t at a known limit, and held there until the run ends.",
        "You want tasks in several projects to keep running after you close the terminal, with one place to answer their questions.",
        "You want another agent to hand off work with `xcb --json route`.",
      ],
    },
    rows: [
      { aspect: "Model access", tool: "A Claude plan, an API key, or a cloud provider such as Amazon Bedrock", xcb: "Your subscriptions, through each provider’s own tool; xcb has no model access of its own" },
      { aspect: "Accounts", tool: "One account per sign-in; switch with `/login`", xcb: "Several accounts across providers; each task goes to one that can take it now" },
      { aspect: "Tools in a run", tool: "Built-in file, shell, and web tools, plus MCP servers, plugins, skills, and subagents", xcb: "xcb’s file tools, offline command VM, and registered host tools replace Claude Code’s built-in tools" },
      { aspect: "Sandboxing", tool: "An optional OS sandbox for shell commands: Seatbelt on macOS, bubblewrap on Linux and WSL2", xcb: "The whole Claude Code process runs in a sandbox with a private home directory" },
      { aspect: "Commands and Git", tool: "Runs commands on your machine and can commit and push", xcb: "Commands run in an offline Linux VM on macOS ARM64; Git is read-only for workers" },
      { aspect: "After you close it", tool: "A local session ends; cloud sessions and routines run in Anthropic’s cloud", xcb: "Managed tasks keep running under a background supervisor on your machine" },
      { aspect: "For other programs", tool: "`claude -p` and the Agent SDK", xcb: xcbFacts.route },
      { aspect: "Platforms", tool: "macOS, Linux, WSL, and Windows", xcb: "Claude runs on Mac ARM64 and on Linux after sandbox setup; Windows provider work uses WSL2" },
    ],
    sections: [
      {
        id: "codex",
        title: "Codex",
        paragraphs: [
          "The same choice applies to Codex, OpenAI’s open-source coding agent for the terminal, IDE, and desktop, which signs in with a ChatGPT plan or an API key. Codex has its own sandbox and approval modes, subagents, skills, MCP servers, and cloud tasks. Use it on its own when one ChatGPT plan covers your work.",
          "xcb runs Codex through its [app-server](https://learn.chatgpt.com/docs/app-server), which OpenAI documents for building Codex into your own product, with ChatGPT device sign-in. Inside an xcb run, Codex’s own shell, web search, and subagents are off, and xcb’s file tools and registered host tools take their place.",
          "xcb runs Codex on macOS only and supports specific Codex builds, adding new ones after they pass its checks, so it can trail the newest Codex release. A signed-in Codex coding session passed on Codex CLI 0.158.0.",
        ],
      },
      {
        id: "codex-plugin",
        title: "OpenAI’s Codex plugin for Claude Code",
        paragraphs: [
          "OpenAI’s [Codex plugin for Claude Code](https://github.com/openai/codex-plugin-cc) adds slash commands that hand work from a Claude Code session to the Codex CLI on the same machine. `/codex:review` runs a read-only Codex review, `/codex:rescue` gives a task to a Codex subagent, and `/codex:status` and `/codex:result` follow background jobs. The plugin uses your existing Codex sign-in, configuration, and checkout, and its work counts toward your Codex usage limits.",
          "Pick the plugin when you work in Claude Code and one Codex sign-in covers the Codex side: it is OpenAI’s own integration, and Codex keeps its own tools and settings. Pick xcb when you hold several Claude or Codex accounts and want each task sent to one that is idle and not at a known limit. Claude Code calls `xcb --json route` from its shell, and xcb chooses the provider, account, and model, runs one turn in its sandbox with xcb’s file tools in place of the provider’s built-in tools, and returns JSON. [Call from an agent](/docs/route#call-from-an-agent) starts with a dry run.",
          "The plugin details come from its README, read on 4 October 2026.",
        ],
      },
      {
        id: "claude-plan",
        title: "Your Claude plan through xcb",
        paragraphs: [
          `${xcbClaudeSignIn} It keeps the resulting token in its private state on your machine and passes it only to Claude Code. ${claudeCodeSignIn} The same page says Pro and Max usage limits “assume ordinary, individual usage of Claude Code and the Agent SDK.”`,
          "xcb runs Claude Code in print mode, and [Anthropic’s help center](https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan) says `claude -p` usage still draws from your plan’s usage limits. Anthropic paused a planned change to that and says it will announce any update before it takes effect. Each task uses the limits of the account it runs on; xcb doesn’t raise or pool them.",
        ],
      },
    ],
    sources: [
      { label: "xcb: registered browser and shared tools", href: "https://github.com/hraness/xcb/blob/1e64357a40fb9b0cb167e9803bc572675100d469/docs/tools.md", checkedOn: "2026-10-01" },
      { label: "Claude Code overview", href: "https://code.claude.com/docs/en/overview", checkedOn },
      { label: "Claude Code sandboxed Bash tool", href: "https://code.claude.com/docs/en/sandboxing", checkedOn },
      { label: "Claude Code routines", href: "https://code.claude.com/docs/en/routines", checkedOn },
      { label: "Claude Code Remote Control", href: "https://code.claude.com/docs/en/remote-control", checkedOn },
      { label: "Claude Code authentication and `claude setup-token`", href: "https://code.claude.com/docs/en/authentication", checkedOn },
      { label: "Anthropic: Claude Code legal and compliance", href: anthropicLegal, checkedOn },
      { label: "Claude help center: the Agent SDK and `claude -p` on your plan", href: "https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan", checkedOn },
      { label: "Codex on GitHub", href: "https://github.com/openai/codex", checkedOn },
      { label: "Codex app-server", href: "https://learn.chatgpt.com/docs/app-server", checkedOn },
      { label: "OpenAI: Codex plugin for Claude Code README", href: "https://github.com/openai/codex-plugin-cc/blob/main/README.md", checkedOn: "2026-10-04" },
      { label: "xcb: Accounts and models", href: "/docs/providers", checkedOn },
      { label: "xcb: Tests, builds, and recovery", href: "/docs/workspace", checkedOn },
    ],
  },
  {
    slug: "conductor",
    glance: [
      { label: "Focus", values: ["Mac workspaces for agents", "Task and account routing"] },
      { label: "Parallel work", values: ["Git worktrees in one repository", "Different projects; one task per project"] },
      { label: "Sandboxed runs", values: [{ status: "no", label: "Local agents run directly" }, { status: "yes", label: "Sandboxed provider runs", detail: "Commands use an offline VM on Mac ARM64." }] },
      { label: "After closing", values: [{ status: "depends", label: "Paid cloud workspaces" }, { status: "yes", label: "Local task supervisor" }] },
      { label: "Review and merge", values: [{ status: "yes", label: "In the app" }, { status: "no", label: "Read-only worker Git" }] },
      { label: "Price", values: ["Free; Pro $50/month", "Free · MIT"] },
    ],
    tool: "Conductor",
    title: "xcb vs Conductor and other parallel-agent workspaces",
    description: "Conductor and similar apps give each agent its own worktree, diff, and merge flow. xcb sends each task to one of your accounts and sandboxes the run.",
    lead: "Conductor and similar apps give each agent its own Git worktree, diff, and merge flow, so you can review parallel attempts at one repository. xcb sends each task to one of your accounts, sandboxes the run, and keeps it going after you close the terminal, but it has no worktree or pull request flow.",
    updated: checkedOn,
    picks: {
      tool: [
        "You want several agents working on one repository at once, each on its own branch.",
        "You review diffs, open pull requests, and merge from one app.",
        "You want Cursor’s agent alongside Claude Code, Codex, and OpenCode.",
        "You want paid cloud workspaces, live collaboration, or a mobile app.",
      ],
      xcb: [
        "You pay for more than one of Claude and Codex, or several accounts at one provider, and want each task on one that can take it now.",
        "You want each run sandboxed instead of running with your user permissions.",
        "You want tasks to keep running on your own machine after you close the terminal.",
        "You want another agent to hand off work with `xcb --json route`.",
      ],
    },
    rows: [
      { aspect: "What it is", tool: "A Mac app with a workspace, branch, terminal, and diff for each agent", xcb: "A router and terminal that sends each task to one of your accounts" },
      { aspect: "Parallel work", tool: "Many agents on one repository, each in its own Git worktree", xcb: "Tasks in different projects run at the same time; tasks in one project run one at a time" },
      { aspect: "Accounts", tool: "The sign-ins already saved on your machine, or API keys", xcb: "Several accounts per provider; one task per account at a time" },
      { aspect: "Sandboxing", tool: "Local agents run directly on your system without sandboxing", xcb: xcbFacts.sandbox },
      { aspect: "Closing the app", tool: "Local sessions end; paid cloud workspaces keep running", xcb: "Managed tasks keep running under a background supervisor" },
      { aspect: "Review and merge", tool: "Diffs, pull requests, and merging in the app", xcb: "None; workers see Git status and diffs read-only" },
      { aspect: "Agents", tool: "Claude Code, Codex, Cursor, and OpenCode", xcb: "Claude Code and Codex" },
      { aspect: "Price", tool: "Free; Pro is $50 a month and adds cloud workspaces, an API, and a mobile app", xcb: "Free and open source under the MIT license" },
    ],
    sections: [
      {
        id: "others",
        title: "Claude Squad, Superset, and Emdash",
        paragraphs: [
          "[Claude Squad](https://github.com/smtg-ai/claude-squad) is a terminal app that runs Claude Code, Codex, Gemini, Aider, and other local agents in separate tmux sessions and Git worktrees, with an optional auto-accept mode. It needs tmux and the GitHub CLI, and it is licensed under AGPL-3.0.",
          "[Superset](https://github.com/superset-sh/superset) runs Claude Code, Codex, or another CLI agent with terminals, code review, and browser previews in one workspace, with a worktree for each task. Its README notes that worktrees “do not sandbox processes or prevent merge conflicts”. It targets macOS, with experimental Linux builds, under the Elastic License 2.0.",
          "[Emdash](https://github.com/generalaction/emdash) is a desktop app for macOS, Windows, and Linux that runs each task in its own worktree, locally or on a remote machine over SSH. It works with Claude Code, Codex, OpenCode, Amp, and more, under the Apache 2.0 license.",
          "Each of them works with the agents and sign-ins you already have. xcb picks an account for each task and sandboxes each run, and it can run beside any of them.",
        ],
      },
    ],
    sources: [
      { label: "Conductor home page", href: "https://www.conductor.build/", checkedOn },
      { label: "Conductor FAQ", href: "https://www.conductor.build/docs/faq", checkedOn },
      { label: "Conductor pricing", href: "https://www.conductor.build/pricing", checkedOn },
      { label: "Claude Squad on GitHub", href: "https://github.com/smtg-ai/claude-squad", checkedOn },
      { label: "Superset on GitHub", href: "https://github.com/superset-sh/superset", checkedOn },
      { label: "Emdash on GitHub", href: "https://github.com/generalaction/emdash", checkedOn },
      { label: "xcb: Accounts and models", href: "/docs/providers#choose", checkedOn },
    ],
  },
  {
    slug: "claude-code-router",
    glance: [
      { label: "Routes", values: ["Individual API requests", "Coding tasks"] },
      { label: "Credentials", values: ["API keys; some subscription imports", "Claude and Codex sign-ins"] },
      { label: "Retry or fallback", values: [{ status: "yes", label: "API requests" }, { status: "depends", label: "Managed usage-limit recovery", detail: "Route calls run once." }] },
      { label: "Run isolation", values: [{ status: "depends", label: "Controlled by your agent" }, { status: "yes", label: "Sandboxed provider runs" }] },
      { label: "Integration", values: ["Local model API endpoint", "JSON task result via CLI"] },
      { label: "Platforms", values: ["Mac, Linux, Windows", xcbPlatforms] },
    ],
    tool: "Claude Code Router",
    title: "xcb vs Claude Code Router and subscription proxies",
    description: "Claude Code Router sends each API request from your agent to a provider you choose. xcb never touches API traffic and hands whole tasks to your own accounts.",
    lead: "Claude Code Router sends each API request from your coding agent to a provider and model you configure, usually with API keys. xcb never touches API traffic: it runs each provider’s own tool under your sign-in and routes whole tasks.",
    updated: checkedOn,
    picks: {
      tool: [
        "You want Claude Code, Codex, OpenCode, or pi to use models from other providers.",
        "You want retries, key rotation, and ordered fallback models for every request.",
        "You want request logs, token counts, and cost estimates in one dashboard.",
        "You pay per token with API keys and want one local endpoint for all your agents.",
      ],
      xcb: [
        "You want the Claude and Codex subscriptions you already pay for used through each provider’s own tool.",
        "You want each task held on one account until its run ends, instead of routing each request.",
        "You want each run sandboxed and a result that says how the run ended.",
      ],
    },
    rows: [
      { aspect: "What it routes", tool: "Each API request from your agent to a provider and model", xcb: "Each coding task to one of your accounts and a model" },
      { aspect: "Where it sits", tool: "A local endpoint between your agents and model APIs", xcb: "Above the agents: it starts Claude Code or Codex itself" },
      { aspect: "What you bring", tool: "API keys, plus logins it can import for some subscriptions", xcb: "Claude and Codex subscriptions, signed in through each provider’s own flow" },
      { aspect: "When a call fails", tool: "Retries, credential pools, key rotation, and ordered fallback models", xcb: "A route call runs once; a managed task can switch accounts after a reported usage limit" },
      { aspect: "Sandboxing", tool: "Your agent runs as usual; the router handles its requests", xcb: "Every provider run is sandboxed, and file changes go through xcb" },
      { aspect: "For your own code", tool: "One local endpoint that compatible API clients can call", xcb: xcbFacts.route },
      { aspect: "Platforms", tool: "Desktop apps for macOS, Windows, and Linux, plus a CLI and Docker", xcb: xcbFacts.platforms },
    ],
    sections: [
      {
        id: "proxies",
        title: "Subscription proxies",
        paragraphs: [
          "Some proxies go further and turn subscription logins into an API. [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) describes itself as a proxy that provides OpenAI-, Gemini-, Claude-, Codex-, and Grok-compatible API interfaces “for CLI”, so any compatible client can use “multiple CLI accounts”.",
          `For Claude, [Anthropic’s Claude Code legal page](${anthropicLegal}) says OAuth sign-in with a Claude plan “is designed to support ordinary use of Claude Code and other native Anthropic applications.” It also says Anthropic does not permit third-party developers “to route requests through Free, Pro, or Max plan credentials on behalf of their users”.`,
          `${xcbClaudeSignIn} It passes the token only to Claude Code. The same page says its rules don’t “prevent an end user from signing in to the unmodified Claude Code binary with their own Claude subscription”.`,
        ],
      },
      {
        id: "building",
        title: "Building on xcb",
        paragraphs: [
          "`xcb --json route` reads one task as JSON and returns one JSON result: the account and model xcb picked, a session you can resume, and how the run ended. The request can pin a provider, account, or model and set a deadline, and it can’t carry tools, credentials, or provider flags.",
          "Apps can embed the TypeScript SDK’s `createSubscriptionRouter` instead. There, your app names the account and model, and xcb holds that account while the task runs. Install the SDK with `npm install @hraness/xcb` or download its release archive. Your app supplies the provider adapters.",
        ],
      },
    ],
    sources: [
      { label: "Claude Code Router on GitHub", href: "https://github.com/musistudio/claude-code-router", checkedOn },
      { label: "CLIProxyAPI on GitHub", href: "https://github.com/router-for-me/CLIProxyAPI", checkedOn },
      { label: "Anthropic: Claude Code legal and compliance", href: anthropicLegal, checkedOn },
      { label: "xcb: Route tasks", href: "/docs/route", checkedOn },
    ],
  },
  {
    slug: "opencode",
    glance: [
      { label: "Focus", values: ["Terminal, IDE, desktop agent", "Router around provider agents"] },
      { label: "Model access", values: ["75+ API providers; local models", "Claude and Codex plans"] },
      { label: "Worker plugins", values: [{ status: "yes", label: "Plugins and MCP" }, { status: "partial", label: "Registered host tools", detail: "No unrelated provider plugins or provider-native shells." }] },
      { label: "Permissions", values: ["Allow, ask, or deny rules", { status: "yes", label: "Sandboxed provider runs", detail: "Commands use an offline VM on Mac ARM64." }] },
      { label: "Platforms", values: ["Mac, Linux, Windows", xcbPlatforms] },
      { label: "License", values: ["MIT", "MIT"] },
    ],
    tool: "OpenCode",
    title: "xcb vs OpenCode: an open agent or a router for your subscriptions",
    description: "OpenCode is an open agent for 75+ providers and no longer bundles Claude Pro/Max plugins. xcb uses your Claude plan through Claude Code, plus Codex.",
    lead: "OpenCode is an open-source coding agent for 75+ providers and local models, and its docs say Anthropic prohibits using a Claude Pro or Max plan through OpenCode plugins. xcb uses your Claude subscription through the unmodified Claude Code binary, along with Codex, routing tasks across the accounts you connect.",
    updated: "2026-10-01",
    picks: {
      tool: [
        "You want one open-source agent across 75+ providers or local models.",
        "You use a ChatGPT Plus, GitHub Copilot, or GitLab Duo subscription, which OpenCode supports with no extra setup.",
        "You want to define your own agents and subagents, with their own prompts, models, and tools.",
        "You want plugins, MCP servers, and language server support in your agent.",
      ],
      xcb: [
        "You want to use a Claude Pro or Max plan through Claude Code, alongside Codex.",
        "You hold several accounts and want each task sent to one that is idle and not at a known limit.",
        "You want every provider run sandboxed, with file changes going through xcb.",
      ],
    },
    rows: [
      { aspect: "What it is", tool: "An open-source coding agent for the terminal, IDE, and desktop", xcb: "A router and terminal around Claude Code and Codex" },
      { aspect: "Model access", tool: "75+ providers through API keys, plus local models", xcb: "Your Claude and Codex subscriptions, through each provider’s own tool" },
      { aspect: "Subscriptions", tool: "ChatGPT Plus, GitHub Copilot, and GitLab Duo; Claude Pro/Max plugins no longer bundled", xcb: "Claude through the unmodified Claude Code binary and ChatGPT through Codex" },
      { aspect: "Customization", tool: "Custom agents and subagents, plugins, MCP servers, and language servers", xcb: "Panes, hooks, and reflex programs; workers use xcb’s tools and registered host MCP servers" },
      { aspect: "Permissions", tool: "Rules that allow, ask about, or deny edits, shell commands, and web fetches", xcb: xcbFacts.sandbox },
      { aspect: "For your own code", tool: "An SDK and a server mode", xcb: xcbFacts.route },
      { aspect: "Platforms", tool: "macOS, Linux, and Windows", xcb: xcbFacts.platforms },
      { aspect: "License", tool: "MIT", xcb: "MIT" },
    ],
    sections: [
      {
        id: "claude-plan",
        title: "Using a Claude subscription",
        paragraphs: [
          "[OpenCode’s provider docs](https://opencode.ai/docs/providers/) say plugins exist that let you use Claude Pro/Max models with OpenCode, that “Anthropic explicitly prohibits this”, and that OpenCode stopped bundling them in 1.3.0. The same page lists ChatGPT Plus, GitHub Copilot, and GitLab Duo as subscriptions you can use with zero setup.",
          `${xcbClaudeSignIn} ${claudeCodeSignIn} Provider terms and usage limits still apply, and xcb doesn’t raise or pool them.`,
        ],
      },
    ],
    sources: [
      { label: "xcb: registered browser and shared tools", href: "https://github.com/hraness/xcb/blob/1e64357a40fb9b0cb167e9803bc572675100d469/docs/tools.md", checkedOn: "2026-10-01" },
      { label: "OpenCode home page", href: "https://opencode.ai/", checkedOn },
      { label: "OpenCode providers", href: "https://opencode.ai/docs/providers/", checkedOn },
      { label: "OpenCode agents", href: "https://opencode.ai/docs/agents/", checkedOn },
      { label: "OpenCode permissions", href: "https://opencode.ai/docs/permissions/", checkedOn },
      { label: "Anthropic: Claude Code legal and compliance", href: anthropicLegal, checkedOn },
      { label: "xcb: Make it yours", href: "/docs/customization", checkedOn },
    ],
  },
  {
    slug: "openrouter",
    glance: [
      { label: "Routes", values: ["Model API requests", "Coding tasks"] },
      { label: "Credentials", values: ["OpenRouter or provider API keys", "Existing subscription sign-ins"] },
      { label: "Hosted endpoint", values: [{ status: "yes", label: "OpenRouter service" }, { status: "no", label: "Local provider CLIs", detail: "Mac ARM64; Claude also runs on Linux after sandbox setup, including in WSL2 on Windows." }] },
      { label: "Cost", values: ["Per-token rates + credit purchase fee", "Free; subscription limits apply"] },
      { label: "Retry or fallback", values: [{ status: "yes", label: "Model and provider chains" }, { status: "depends", label: "Managed usage-limit recovery", detail: "Route calls run once." }] },
    ],
    tool: "OpenRouter",
    title: "xcb vs OpenRouter: your subscriptions or a per-token API",
    description: "OpenRouter bills per token for API calls to hundreds of models. xcb sends each coding task to a Claude or Codex plan you already pay for.",
    lead: "OpenRouter is a hosted API that sends each request from your code to one of hundreds of models and bills you per token. xcb runs on your machine and sends each coding task to one of the Claude or Codex accounts you already pay for; it never proxies or rewrites API traffic.",
    updated: checkedOn,
    picks: {
      tool: [
        "You want one API key and one bill across hundreds of models.",
        "You want automatic fallbacks when a provider rate-limits or fails.",
        "You need a model your subscriptions don’t include, or image, video, or speech models.",
        "Your app already calls the OpenAI chat API and you want to switch models without changing code.",
      ],
      xcb: [
        "You already pay for Claude or Codex and want coding tasks to use those plans, with no per-token charge from xcb.",
        "You want each task held on one account until its run ends; if xcb can’t confirm how a run ended, it keeps the account held and doesn’t retry.",
        "You want your account credentials to stay on your machine, outside your project folder.",
      ],
    },
    rows: [
      { aspect: "What it routes", tool: "One API request: a prompt in, a model’s answer out", xcb: "One coding task; each route call runs one provider turn on one model" },
      { aspect: "What you bring", tool: "An OpenRouter API key, or your own provider keys through BYOK", xcb: "Claude or Codex subscriptions you already pay for, signed in on your machine" },
      { aspect: "Where it runs", tool: "OpenRouter’s hosted service; requests leave your machine for its endpoint", xcb: "Your machine: both providers on Mac ARM64; Claude on Linux after sandbox setup, or through WSL2 on Windows" },
      { aspect: "How you pay", tool: "Per token at the provider’s price, plus a 5.5% fee when you buy credits on the Standard plan", xcb: "xcb is free; usage draws on each subscription’s own limits" },
      { aspect: "Choosing the model", tool: "The model field on each request, or the `openrouter/auto` router", xcb: "xcb picks from models recently seen on accounts that can take the task now" },
      { aspect: "Choosing the provider", tool: "Provider order, price ceilings, and region limits on each request", xcb: "The provider of the chosen account; each turn runs on one account" },
      { aspect: "When a call fails", tool: "Fallback chains retry on another model or provider", xcb: "A route call runs once; a managed task can switch accounts after a reported usage limit" },
    ],
    note: `OpenRouter’s pricing page lists 500+ models from 80+ providers. Requests on your own provider keys are free up to $25,000 of list-price inference a month, then carry a 5% fee. OpenRouter also serves image, video, and speech models, which this table leaves out.`,
    sources: [
      { label: "OpenRouter home page", href: "https://openrouter.ai/", checkedOn },
      { label: "OpenRouter pricing", href: "https://openrouter.ai/pricing", checkedOn },
      { label: "OpenRouter FAQ: pricing and fees", href: "https://openrouter.ai/docs/faq", checkedOn },
      { label: "OpenRouter routing guide", href: "https://openrouter.ai/blog/insights/model-routing", checkedOn },
      { label: "OpenRouter multimodal capabilities", href: "https://openrouter.ai/docs/guides/overview/multimodal/overview", checkedOn },
      { label: "xcb: Route tasks", href: "/docs/route", checkedOn },
    ],
  },
];

export function comparisonPath(slug: string): string {
  return `/compare/${slug}`;
}

export function comparisonHeading(entry: Comparison): string {
  return entry.heading ?? `xcb vs ${entry.tool}`;
}

export function findComparison(slug: string): Comparison | undefined {
  return comparisons.find((entry) => entry.slug === slug);
}
