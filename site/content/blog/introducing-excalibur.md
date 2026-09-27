Excalibur, or xcb, routes coding tasks across the Claude, Codex, and Devin subscriptions you already pay for. You can type tasks into its terminal or have another agent hand them over as JSON, and either way xcb runs each task in the provider's own coding tool, under your own sign-in.

Paying for more than one coding agent means more than one of everything around it: a terminal for each provider, a sign-in and usage limits for each account, and sessions scattered across windows. Before each task you check which account is free, and afterward you look through those windows for the session that was fixing a flaky test.

## How xcb runs each task

xcb reaches each model through the provider's own tool: Claude Code, Codex, or the Devin CLI. You sign in through that tool's own flow, and xcb keeps the credentials in a private profile outside your projects. It runs a private copy of the tool you installed, and only builds that pass xcb's checks.

For each task, xcb picks an account that is signed in, idle, and not at a known usage limit, on a model from that provider's current model list. Known limits come from Claude's five-hour and seven-day usage windows; xcb does not infer Codex or Devin limits. While the task runs, xcb holds that account so no other task can start on it, and it releases the account only after the provider process has exited. The provider runs in an operating-system sandbox (Seatbelt on macOS, bwrap on Linux) with a private home directory and a cleared environment, and it changes your files through xcb. When the run ends, xcb records how it ended; if it cannot confirm that, it keeps the account held and does not retry.

If a task from your thread stops on a usage limit that the provider reports, xcb can move it to another account that is free, along with its original instructions. xcb has no model access of its own and does not raise any account's usage limits. Each provider's terms and limits apply to every task xcb runs.

## Work from one thread across your projects

Plain `xcb` opens your thread, one conversation per machine that spans your projects. Type a task and xcb picks the project directory it runs in and says why, for example “Started Fix the parser in `app` · named `app` · /workspace to move”. When it cannot tell which project you mean, it asks and saves nothing until you pick.

Each prompt becomes a task that keeps running after you close the terminal, because a background supervisor runs it. `/tasks` lists the work, `/attention` collects the questions and approvals your agents are waiting on, and `/steer <task-id> <guidance>` queues guidance for a task's next turn. Tasks in different projects can run at the same time on different accounts; tasks in the same project run one at a time.

If you use one subscription, or you rely on a provider's plugins, MCP servers, skills, subagents, or web search, the provider's own tool serves you better. Those features are not available in the runs xcb starts, where the provider works on your files through xcb's own tools.

## Hand tasks to xcb from another program

Another program, usually a coding agent, can give xcb one task with `xcb --json route`. It writes one JSON request to stdin:

```json
{ "version": 1, "workspace": "/absolute/path/to/project", "task": "Fix the failing parser test and show the diff" }
```

xcb picks the account and model, runs exactly one provider turn, and writes one JSON result to stdout once the provider process has exited:

```json
{
  "version": 1,
  "status": "completed",
  "requestId": "route_…",
  "session": "s_…",
  "route": { "provider": "claude", "account": "a_…", "model": "claude/sonnet/low", "label": "Sonnet · low", "reason": "…" },
  "state": "idle",
  "outcome": { "terminal": "completed", "joined": true, "effects": "settled", "pending_attention": false, "failure": null },
  "text": "…"
}
```

The request can also pin a provider, account, or model, set a deadline, or ask for a dry run that shows the chosen route without reserving an account. It never accepts tools, hooks, system prompts, credentials, or provider flags. Returned text is capped at 256 KiB. A failure exits with a nonzero code and names a reason such as `busy` or `needs_input`; `custody_unproven` means xcb could not confirm how the run ended, so the account stays held and the caller should not retry blindly.

Applications can embed the TypeScript SDK instead. There the application names the account and model for each task and supplies the provider adapters, and `createSubscriptionRouter` holds that account while the task runs. The [route documentation](/docs/route) covers both.

## How xcb compares

[Herdr](/compare/herdr) keeps your coding agents running, each in its own terminal, and marks which one is waiting on you; xcb decides which of your accounts runs each task, on which model, and holds that account until the run ends. Like [Pi](/compare/pi), a minimal coding agent you adapt with extensions, xcb can be reshaped: panes change what its terminal shows, hooks run your own programs when sessions and turns start or end, and reflexes learn from your replies and can be rolled back. None of them can give a provider run more access, and because hooks run with your own permissions, each one stays off until you turn it on. The [comparison page](/compare) covers other tools.

## Limits

With the tested accounts, Claude and Devin have completed coding tasks through xcb on macOS on Apple silicon. Claude ran a failing test, fixed the code, passed the test, and checked Git status; Devin, on Devin CLI 3000.11.3, fixed a broken function and reported the change. On Linux only Claude runs, in a bwrap sandbox once xcb's checks pass, and a coding session there has not been confirmed. Codex and Devin need macOS, and Codex runs only on specific builds that xcb has checked; its last signed-in coding run used the previous supported build. The [providers page](/docs/providers) lists each supported build and its status.

Tests and builds run only in an offline Linux VM that you set up on macOS on Apple silicon; without it, agents can read and change files but cannot run commands. Git in that VM is read-only, so agents working through xcb do not commit or push, and native macOS or Xcode builds cannot run there.

The managed harness that runs the tasks in your thread is experimental. It is being rebuilt so that it can propose routing rules, test them on labeled examples, and keep a rule only when it scores better than the current one. That work is in development, and the current build does not run self-modifying routing policies.

## Install xcb and connect Claude

Latest release: {{release.version}}. Release binaries are built for macOS on Apple silicon and Linux x86_64, and one line installs xcb:

```sh
curl -fsSL https://xcb.sh/install.sh | sh
```

With Claude Code installed, `xcb setup claude` adds a Claude account or reuses yours, checks your Claude Code build, opens the browser sign-in, and loads the account's models. Then plain `xcb` opens your thread:

```sh
xcb setup claude
xcb
```

To build from source, or to connect Codex or Devin, follow the [getting started guide](/docs/getting-started).
