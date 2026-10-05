Excalibur (xcb) routes coding tasks across the Claude and Codex subscriptions you already pay for. Give it a task from your terminal or from another agent, and it chooses an available account and a model for the work. Each task runs through the provider's own coding tool under your sign-in.

With several coding plans, choosing where to send the next task becomes a job of its own. One account is busy, another is close to its limit, and a third has unused quota that resets soon. xcb makes that choice for each task and keeps a local record of the work, so you or your agent can check on it later.

## Give xcb a task

`xcb run -p "Fix the failing parser test"` runs one task in the current project folder and prints the answer when it finishes. For work that should keep going after the command returns, add it to a project's backlog:

```sh
xcb backlog add ~/src/app "Fix the failing parser test" --ready
```

xcb's background supervisor runs backlog tasks, so they can outlive the command that submitted them. `xcb tasks` shows the work across your projects, `xcb attention` collects questions waiting for your answer, and `xcb steer <task-id> <guidance>` adds instructions for a task's next turn. Different projects can run at the same time on different accounts; tasks in the same project take turns.

## Choose an account for the work

xcb first finds accounts that are signed in, idle, and able to take the task. It then ranks their models for the work. Fresh Claude and Codex usage reports let it favor unused quota approaching a reset while preserving the task's quality requirements and any provider, account, or model you chose.

The selected account stays reserved until the provider process exits. If xcb cannot confirm how a run ended, it keeps the account reserved instead of retrying. When a provider reports a usage limit during a managed task and the run ends cleanly, xcb can continue on another available account with the original instructions.

That makes existing capacity easier to use. Each account still has its provider's usage limits. The [routing guide](/docs/how-routing-works) explains the selection rules and how to inspect a route before starting it.

## Keep the provider and the project separate

xcb stores provider credentials outside your projects and starts supported provider builds in an operating-system sandbox. Project file access goes through xcb's tools. Native provider shells and unrelated plugins are unavailable inside those runs; registered host tools have their own permissions and setup.

If one coding plan covers your work and you want that provider's complete native toolset, its own coding tool may already suit you. xcb is useful when choosing among accounts, keeping tasks running, or handing work between programs is part of your day.

Platform support also matters. The [accounts guide](/docs/providers) lists supported provider builds. The [command runner](/docs/workspace) uses an offline Linux VM on macOS with Apple silicon for tests and builds; native macOS builds cannot run in that VM. The self-tuning managed harness remains in development and does not run self-modifying routing policies.

## Hand over a task from another agent

An agent or script can send one JSON request to `xcb --json route`:

```json
{
  "version": 1,
  "workspace": "/absolute/path/to/project",
  "task": "Fix the failing parser test and show the diff"
}
```

xcb chooses the account and model, runs one provider turn, and returns the route, outcome, and answer as JSON. A dry run can show the proposed route without reserving an account. The [route reference](/docs/route) covers the request and result.

An application can instead use the TypeScript SDK. In that interface, the application chooses the account and model, and xcb holds the account while the task runs. The [SDK guide](/docs/sdk) has a complete example.

## Connect your first account

Latest release: {{release.version}}. Follow the [installation guide](/install) for your platform. With Claude Code installed, connect an account and run a first task from a project folder:

```sh
xcb setup claude
xcb run -p "Explain this repository"
```

Setup checks the provider build, opens sign-in, and loads the account's models. On macOS with Apple silicon and Linux x86_64, the installer also adds aicharts, which keeps a daily record of your agents' token use on your computer for `xcb usage` to read; [How xcb uses aicharts](/blog/how-xcb-uses-aicharts) covers it. The [getting started guide](/docs/getting-started) walks through the first task.
