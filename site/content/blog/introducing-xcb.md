xcb, short for Excalibur, routes coding tasks across the Claude, Codex, and Devin subscriptions you already pay for. You give it a task. It picks one of your accounts that is signed in, idle, and not at a known quota limit, and it holds that account so no other task can use it until the provider's process has exited.

## Why route tasks across subscriptions

If you pay for more than one coding agent, each comes with its own terminal, sign-in, and quota, and keeps long sessions in its own place. The work ends up scattered: one account hits its limit halfway through a refactor, a second sits idle, and nobody remembers which window holds the session that was fixing the flaky test.

xcb puts one router in front of those subscriptions. Each task goes to an account that can take it now, long sessions are trimmed to fit the model's context, and every managed task leaves a local record you can replay offline.

## Who xcb is for

xcb is for developers who already use more than one of Claude Code, Codex, and Devin and want one workflow around them. Other agents can call `xcb --json route` with one JSON task and read one JSON result; that route picks the account and model itself. Application builders can embed the TypeScript SDK, where the application names the account and model and the router holds that account while the task runs.

If you use one agent on one subscription, the provider's own tool serves you better. The same goes if you need a full replacement for Codex, Claude Code, or Devin today; the limits are at the end of this post.

## Your first managed conversation

The native Rust app runs a terminal workspace on top of the router. With a supported Claude Code build installed, the first managed conversation looks like this:

```sh
xcb accounts add claude --plan Max
xcb doctor --provider claude
xcb accounts login <account-id>
xcb accounts refresh <account-id>
xcb models
xcb --cwd /absolute/path/to/your/project
```

xcb runs its own sign-in and does not silently import your existing provider login. The `--plan` flag is a display label; it does not check your subscription. Prompts in that conversation become tasks that outlive the terminal: closing it detaches from them without cancelling them. `/tasks` lists the work, `/attention` collects questions and approvals across agents, and `/steer <task-id> <guidance>` queues guidance for a task's next turn. Tasks in the same workspace run one at a time; independent workspaces and accounts run side by side.

Native binaries are published for macOS on Apple silicon and for Linux x86_64, each with a checksum. Other hosts build from source. The [getting started guide](/docs/getting-started) covers Codex and Devin sign-in.

### Long sessions stay inside context

Context management is on by default. For Claude Code and Codex sessions, once a prompt passes a size threshold, xcb applies Gobstopper's elision policy to old tool output. The replaced output becomes a short marker that says how many bytes were elided and that the original is kept in local history. The eight most recent tool outputs are never elided. With the optional judge turned on, each candidate is checked first, and output the next turn still needs stays in full. The judge is an external service (TypeSafe System One) that you enable with your own key; it receives a limited excerpt of the task, but not the old tool output it rules on. Devin sessions are not trimmed. `xcb plugins disable gobstopper` turns this off. The details are in [How xcb uses Gobstopper](/blog/how-xcb-uses-gobstopper).

### Reflexes act only after your replies back them

Some decisions come up many times a day: whether a task needs a frontier model, whether a worker that stopped should keep going, whether “Should I merge it?” deserves a yes. xcb calls these reflexes, and it learns them from how you answer. When you reply “continue” to a finished turn, that labels the turn as one that stopped short. A reply of “yes, go ahead” labels a turn that asked for approval.

The continuation reflexes start out observing. Since v0.6.0 they default to `auto`, which means they act only after your own replies show they are precise enough. xcb replays your labeled turns, and a reflex may act only at a threshold where it fired on at least 30 turns and the lower bound of its measured precision reaches a floor: 0.75 for continuing a stopped turn and 0.85 for answering “yes” on your behalf. About one such turn in ten is still left to you, so your replies keep measuring it, and a reflex whose precision drops goes back to observing. A request that mentions deletion, credentials or secrets, production, or spending is never answered “yes” automatically. A reflex learns your preferences and does not measure model quality, and its precision floor is a guardrail, not a guarantee of a correct call. The [reflexes reference](/docs/reflexes) documents the full rule.

### Task history you can replay offline

xcb uses ALGAL to record managed task transitions and to run the small planning programs behind scheduled work. Evaluating one of those programs uses no provider, subprocess, or network. A controller that needs a worker suspends at that request, xcb saves the checkpoint, and the worker starts only through the same project permissions as any other task. `xcb schedules program --managed-calls 2` lets a controller request up to two worker tasks; the limit is eight. `xcb tasks verify <task-id>` replays the task's local record offline and checks it against the stored task. It cannot confirm what a provider claimed or what happened outside your machine. [Replayable task history](/blog/replayable-task-history) explains the method, and [How xcb uses ALGAL](/blog/how-xcb-uses-algal) shows where it runs.

Workers can also search a Wordcell vault once you bind it to their project directory with `xcb memory configure <dir>`. Results come back cited, and saving a note to the vault is always a separate step. See [How xcb uses Wordcell](/blog/how-xcb-uses-wordcell).

## Where xcb is going

The managed harness is being rebuilt on ALGAL so that it can propose routing rules, test them on labeled examples, and keep a rule only when it scores strictly better. Provider checks, account locking, and run records stay fixed. This work is in development, and the current build does not run self-modifying routing policies.

## What it does not do yet

xcb is not yet a daily-driver replacement for Codex, Claude Code, and Devin. Installed Claude and Codex coding workflows have been confirmed on macOS on Apple silicon with the tested accounts, and Codex support is for one exact build. The Devin builds xcb supports have passed sandbox checks run without signing in; a coding session on a signed-in Devin account has not been confirmed. The native Codex and Devin paths need macOS sandboxing, and Claude on Linux is not yet confirmed. Project commands run in an isolated Linux VM that you set up on macOS on Apple silicon, with read-only Git status and diffs, so commit, push, and native macOS or Xcode builds are unavailable there.
