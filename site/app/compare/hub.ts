import type { ArticleIsoDate } from "@hraness/design-kit";
import { comparisonPath, devinStatus } from "./comparisons";

/**
 * The words on /compare. Each group names a kind of tool people weigh against
 * xcb, says how xcb differs in a sentence or two, and lists the tools buyers
 * name most. A tool with its own comparison page links there; the others link
 * to their own site or documentation. Text fields accept the same inline marks
 * as `comparisons.ts`.
 */

export interface HubTool {
  readonly name: string;
  readonly href: string;
  /** One line describing the tool from its own documentation. */
  readonly summary: string;
}

export interface HubGroup {
  readonly id: string;
  readonly title: string;
  readonly summary: string;
  readonly tools: readonly HubTool[];
}

export const hubTitle = "How xcb compares with coding agents, terminals, and routers";
export const hubDescription = "xcb runs each coding task on one of your own Claude, Codex, or Devin subscriptions. See where it fits beside agents, agent terminals, and routers.";
export const hubHeading = "How xcb compares";
export const hubLead = "xcb runs each coding task on one of your own Claude, Codex, or Devin subscriptions, through that provider’s own tool. It holds the account until the run ends, sandboxes the run, and returns the result to you or to another agent.";
/** The day the tool descriptions below were last read against their sources. */
export const hubUpdated: ArticleIsoDate = "2026-09-28";

export const hubGroups: readonly HubGroup[] = [
  {
    id: "provider-agents",
    title: "Provider agents",
    summary: `Claude Code, Codex, and the Devin CLI are the coding agents themselves, and xcb runs them for you under your own accounts. Use one directly when a single subscription covers your work, and add xcb when you pay for more than one. ${devinStatus}`,
    tools: [
      { name: "Claude Code", href: comparisonPath("claude-code"), summary: "Anthropic’s coding agent for the terminal, IDE, desktop, and browser." },
      { name: "Codex", href: `${comparisonPath("claude-code")}#codex`, summary: "OpenAI’s open-source coding agent, signed in with a ChatGPT plan or an API key." },
      { name: "Devin CLI", href: "https://docs.devin.ai/cli", summary: "Cognition’s local coding agent, which can hand work off to Devin Cloud." },
    ],
  },
  {
    id: "harnesses",
    title: "Harnesses and open agents",
    summary: "These let you change the agent itself or point it at many models. xcb sits above the providers’ own agents: you can reshape its terminal, hooks, and routing, and every provider run stays sandboxed. What xcb learns today is limited to two reflexes; a harness that tunes its own routing rules is in development, and the current build does not run self-modifying routing policies.",
    tools: [
      { name: "pi", href: comparisonPath("pi"), summary: "A minimal coding agent you extend with TypeScript, with 15+ model providers." },
      { name: "OpenCode", href: comparisonPath("opencode"), summary: "An open-source coding agent for 75+ providers and local models." },
      { name: "Goose", href: "https://goose-docs.ai/docs/guides/acp-providers/", summary: "An open-source agent that can use your Claude Code or ChatGPT subscription through ACP providers." },
      { name: "Prime Agent", href: "https://github.com/PrimeIntellect-ai/prime-agent", summary: "An open-source coding agent whose harness refines its own prompts, memories, and subagent specs." },
    ],
  },
  {
    id: "terminals",
    title: "Agent terminals and multiplexers",
    summary: "These keep many agent sessions running and visible, each agent under its own sign-in. xcb decides which account runs each task, and it runs inside any of them, including a herdr pane.",
    tools: [
      { name: "herdr", href: comparisonPath("herdr"), summary: "Keeps coding agents running in terminals you can reattach to, and shows which one needs you." },
      { name: "cmux", href: "https://github.com/manaflow-ai/cmux", summary: "A Ghostty-based macOS terminal with vertical tabs and notifications for coding agents." },
    ],
  },
  {
    id: "workspaces",
    title: "Parallel workspaces",
    summary: "These give each agent its own Git worktree, diff, and merge flow, so you can review parallel attempts at one repository. xcb has no worktree or pull request flow; it spreads tasks across your accounts and sandboxes each run.",
    tools: [
      { name: "Conductor", href: comparisonPath("conductor"), summary: "A Mac app that runs Claude Code, Codex, Cursor, and OpenCode in parallel workspaces." },
      { name: "Claude Squad", href: `${comparisonPath("conductor")}#others`, summary: "A terminal app that runs several agents in tmux sessions and Git worktrees." },
      { name: "Superset", href: `${comparisonPath("conductor")}#others`, summary: "A desktop workspace for CLI agents with terminals, code review, and browser previews." },
      { name: "Emdash", href: `${comparisonPath("conductor")}#others`, summary: "A desktop app that runs each agent task in its own worktree, locally or over SSH." },
      { name: "OpenChamber", href: "https://github.com/openchamber/openchamber", summary: "An open-source workspace for running and reviewing agent work on desktop, web, VS Code, and mobile." },
      { name: "CLI Agent Orchestrator", href: "https://github.com/awslabs/cli-agent-orchestrator", summary: "Runs Claude Code, Codex, Kiro, and other CLIs in separate terminal sessions, with a supervisor agent handing work to workers." },
    ],
  },
  {
    id: "account-switchers",
    title: "Account switchers",
    summary: "These change which login your usual Claude Code or Codex uses, by hand or before a usage limit, and you keep every feature of the provider’s tool. xcb keeps each account in its own private profile, picks one per task across Claude, Codex, and Devin, and replaces the provider’s built-in tools with its own file tools in each run.",
    tools: [
      { name: "claude-swap", href: "https://github.com/realiti4/claude-swap", summary: "Switches Claude Code between saved logins and can rotate before you hit a rate limit." },
      { name: "aisw", href: "https://github.com/burakdede/aisw", summary: "Named account profiles for Claude Code, Codex CLI, Gemini CLI, and Antigravity CLI." },
      { name: "CLAUDE_CONFIG_DIR", href: "https://code.claude.com/docs/en/env-vars", summary: "Claude Code’s own environment variable for keeping each account’s settings and sessions in a separate folder." },
    ],
  },
  {
    id: "routers",
    title: "Subscription routers and proxies",
    summary: "Request routers forward each API call from your agent to a provider you choose, and some proxies turn subscription logins into an API. xcb doesn’t touch API traffic: it runs each provider’s own tool under your sign-in and routes whole tasks.",
    tools: [
      { name: "Claude Code Router", href: comparisonPath("claude-code-router"), summary: "A local gateway that routes your agents’ API requests to providers you configure." },
      { name: "CLIProxyAPI", href: `${comparisonPath("claude-code-router")}#proxies`, summary: "A proxy that offers OpenAI-, Gemini-, and Claude-compatible APIs backed by CLI accounts." },
      { name: "CC Switch", href: "https://github.com/farion1231/cc-switch", summary: "Switches the API provider behind Claude Code, Codex, and other agents in one click." },
      { name: "Claudexor", href: "https://claudexor.ai/", summary: "Also runs agents through their own CLIs: Codex CLI, Claude Code, Cursor CLI, OpenCode, and Antigravity CLI, under named subscription profiles with opt-in rotation away from a spent account." },
    ],
  },
  {
    id: "gateways",
    title: "Metered model gateways",
    summary: "These give your code one API for many models, billed per token or through your own keys. xcb uses subscriptions you already pay for and has no model catalog of its own.",
    tools: [
      { name: "OpenRouter", href: comparisonPath("openrouter"), summary: "A hosted API for 500+ models, billed per token from prepaid credits." },
      { name: "LiteLLM", href: "https://github.com/BerriAI/litellm", summary: "An open-source, self-hosted gateway that calls 100+ model APIs in the OpenAI format." },
    ],
  },
  {
    id: "remote",
    title: "Remote and mobile control",
    summary: "These let you steer agents on your computer from a phone or browser. xcb can dispatch and steer tasks on your other machines from its CLI, through an encrypted relay you deploy yourself; it has no phone or browser app.",
    tools: [
      { name: "Happy", href: "https://github.com/slopus/happy", summary: "A mobile and web client for Claude Code and Codex with end-to-end encryption." },
      { name: "Paseo", href: "https://paseo.sh/", summary: "One interface for Claude Code, Codex, Copilot, OpenCode, and Pi agents, from desktop, phone, or web." },
      { name: "Claude Code Remote Control", href: "https://code.claude.com/docs/en/remote-control", summary: "Continues a local Claude Code session from your phone, tablet, or a browser." },
    ],
  },
];

/** Where xcb's own limits send you to another tool. */
export const hubElsewhere: readonly string[] = [
  "You pay for one subscription and want every native feature: use that provider’s own tool.",
  "The agent needs to commit, push, or build for macOS: in xcb, Git is read-only for workers, and commands run in an offline Linux VM on macOS ARM64.",
  "You need providers to run natively on Windows, or Codex or Devin on Linux: Windows provider work requires xcb’s Linux build in WSL2, and Codex and Devin need macOS.",
  "You want web search, MCP servers, or plugins inside the agent: xcb turns those off in its runs, so use the provider’s tool or an open agent.",
];
