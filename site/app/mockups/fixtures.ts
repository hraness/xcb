/**
 * Sample data for the xcb illustrations on the home page, in the launch post,
 * and in the launch film. Account names, projects, machines, and tasks are
 * made up. Each line copies the shape the CLI prints:
 *
 * - `xcb accounts` status cells: crates/xcb-cli/src/health.rs `status`
 * - thread start notice: crates/xcb-runtime (`Started … in … · named …`)
 * - running and limit lines: crates/xcb-runtime/src/managed.rs
 * - `xcb tasks`: crates/xcb-cli/src/main.rs; `xcb attention`: habitat.rs
 * - `xcb fleet` and `xcb dispatch`: crates/xcb-cli/src/remote.rs
 * - route request and result: docs/route.md
 *
 * tests/launch.test.ts reads those sources and fails when a shape drifts.
 */

export const illustrationCaption = "Illustration. Accounts, projects, and tasks are made up.";

/** The two moments the quota board, thread, and task list show. */
export type RouterMode = "reset" | "limit";
export const routerModes: readonly RouterMode[] = ["reset", "limit"];

export type BoardAccount = Readonly<{
  id: string;
  name: string;
  provider: "claude" | "codex" | "devin";
  plan: string;
  /** Percent of the current usage window left, or null when the provider reports no meter. */
  left: number | null;
  /** The STATUS cell exactly as `xcb accounts` words it. */
  status: string;
}>;

/**
 * One board per moment. In "reset" xcb picks the Claude account whose unused
 * quota resets soonest. In "limit" that account has just hit its limit, and
 * the task moves to the other Claude account.
 */
export const boardAccounts: Readonly<Record<RouterMode, readonly BoardAccount[]>> = {
  reset: [
    { id: "a_3f9c0e12…", name: "work", provider: "claude", plan: "Max", left: 62, status: "ready · 62% left, resets in ~38m" },
    { id: "a_7b21d4aa…", name: "personal", provider: "claude", plan: "Max", left: 81, status: "ready · 81% left, resets in ~4d" },
    { id: "a_c0de5a77…", name: "chatgpt", provider: "codex", plan: "Plus", left: 45, status: "busy · 45% left, resets in ~2h 10m" },
    { id: "a_d0e1f2a3…", name: "team", provider: "devin", plan: "Team", left: null, status: "ready" },
  ],
  limit: [
    { id: "a_3f9c0e12…", name: "work", provider: "claude", plan: "Max", left: 0, status: "limited · retry in ~1h 52m" },
    { id: "a_7b21d4aa…", name: "personal", provider: "claude", plan: "Max", left: 79, status: "busy · 79% left, resets in ~4d" },
    { id: "a_c0de5a77…", name: "chatgpt", provider: "codex", plan: "Plus", left: 45, status: "busy · 45% left, resets in ~2h 4m" },
    { id: "a_d0e1f2a3…", name: "team", provider: "devin", plan: "Team", left: null, status: "ready" },
  ],
};

/** The account each moment gives the parser task to. */
export const boardPick: Readonly<Record<RouterMode, string>> = { reset: "a_3f9c0e12…", limit: "a_7b21d4aa…" };

export const boardNote: Readonly<Record<RouterMode, string>> = {
  reset: "Both Claude accounts can take the task. work still has 62% left and resets in 38 minutes, so it goes first instead of letting that quota lapse.",
  limit: "work hit its 5-hour limit mid-task. The task keeps its instructions and continues on personal.",
};

export const sampleTask = "Fix the failing parser test in invoice-app";

/** The thread after you type the task. `tone` colours the line; the text is what xcb shows. */
export type ThreadLine = Readonly<{ who: "you" | "xcb"; text: string; tone?: "ok" | "warn" | "muted" }>;

const started = "Started Fix the failing parser test in invoice-app · named invoice-app · /workspace to move";
const answer = "Two-digit years now parse as 20xx. The parser tests pass.";

export const threadLines: Readonly<Record<RouterMode, readonly ThreadLine[]>> = {
  reset: [
    { who: "you", text: sampleTask },
    { who: "xcb", text: started, tone: "muted" },
    { who: "xcb", text: "worker is running · quota leader · claude/opus · a_3f9c0e12", tone: "ok" },
    { who: "xcb", text: answer },
  ],
  limit: [
    { who: "you", text: sampleTask },
    { who: "xcb", text: started, tone: "muted" },
    { who: "xcb", text: "Usage limit interrupted claude/opus · a_3f9c0e12; selecting another eligible route", tone: "warn" },
    { who: "xcb", text: "worker is running · quota leader · claude/opus · a_7b21d4aa", tone: "ok" },
    { who: "xcb", text: answer },
  ],
};

/** `xcb tasks` rows: `<id>  <state> · <title> · <detail> · <route>`. */
export const taskRows: Readonly<Record<RouterMode, readonly string[]>> = {
  reset: [
    "t_8d21  running · Fix the failing parser test · worker is running · quota leader · claude/opus · a_3f9c0e12",
    "t_77c0  needs input · Rename the billing module · Which name: billing or invoicing? · codex/gpt-codex · a_c0de5a77",
    "t_5a93  completed · Add a CSV export · Export added with tests · devin/swe · a_d0e1f2a3",
  ],
  limit: [
    "t_8d21  running · Fix the failing parser test · worker is running · quota leader · claude/opus · a_7b21d4aa",
    "t_77c0  needs input · Rename the billing module · Which name: billing or invoicing? · codex/gpt-codex · a_c0de5a77",
    "t_5a93  completed · Add a CSV export · Export added with tests · devin/swe · a_d0e1f2a3",
  ],
};

/** `xcb attention` row: `<id> · <conversation> · <state> · <detail>`, then the last output. */
export const attentionRows = [
  "t_77c0 · thread · needs input · Which name: billing or invoicing?",
  "  Both names appear in the code. I can rename either way.",
] as const;

/** `xcb fleet`: `<device> · <label> · <class> · <online|offline> · <status>`, then projection rows. */
export const fleetRows = [
  "d_513c · studio-desktop · daemon · online · active",
  "  projection thread · rev 41 · 3s old",
  "d_a90e · travel-laptop · controller · online · active",
  "d_2f47 · build-box · daemon · offline · active",
  "  projection thread · rev 12 · 5400s old · STALE",
] as const;

export const dispatchCommand = "xcb dispatch d_513c ~/src/invoice-app -p \"Run the parser tests and fix what fails\"";
export const dispatchResult = "Posted to d_513c as c_91ad (idempotency k_3e70).";

/** A route request and a trimmed result, in the shape docs/route.md documents. */
export const routeRequest = `{
  "version": 1,
  "workspace": "/home/you/src/invoice-app",
  "task": "Fix the failing parser test"
}`;

export const routeResponse = `{
  "version": 1,
  "status": "completed",
  "route": {
    "provider": "claude",
    "account": "a_7b21d4aa…",
    "model": "claude/opus"
  },
  "outcome": {
    "terminal": "completed",
    "effects": "settled"
  },
  "text": "Two-digit years now parse as 20xx."
}`;
