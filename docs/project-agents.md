# Persistent project agents

Project agents remain a local durable capability, but the former terminal
thread/TUI entry point is removed. Use the JSON projections and SDK described
in [route.md](route.md) and [vision.md](vision.md) for agent interaction.

A project is a workspace directory. Its backlog, work history, autonomy grant,
schedules, working memory, and Wordcell binding belong to that directory. Your
thread (plain `xcb`) and every project view over the directory (`xcb chat
--new`) share them, so the same goal and history follow the project whichever
conversation you use. Each task is routed automatically to an account and model
that can take it; large prompts get the highest known quality, and a usage limit
that forces a lower-ranked route produces a warning. Model selection normally
needs no input from you.

Commands below take `<dir|name>`: a directory path (anything containing `/`,
or `.` and `..`), a registered project name from `xcb workspaces`, or a
directory relative to the current one. A project view's conversation ID from
`xcb conversations` is accepted as another name for that view's directory; the thread's ID is refused, because the thread
spans projects.

## Work, questions and approvals

```sh
xcb backlog --workspace <dir>
xcb backlog add <dir|name> "Review the next milestone" --priority 7
xcb backlog edit <task-id> "Review authentication" --revision 1
xcb backlog release <task-id> --revision 2
xcb backlog complete <deferred-task-id> "Already covered by the passing parser tests" --revision 1
xcb attention
xcb backlog reply <task-id> "Use the existing project conventions"
xcb backlog reconcile <uncertain-task-id> --revision 4
xcb backlog dismiss <uncertain-task-id> --revision 4
```

`backlog reconcile` closes uncertain work only when xcb kept proof of how it
ended. When there is no proof, check the project yourself, then
`backlog dismiss` the task: it is marked failed, nothing is retried, and the
project's schedules can start again. Dismissal waits until the task's worker
has stopped.

`xcb backlog --workspace <dir>` lists work in that directory from every
conversation; `--conversation <conversation-id>` filters by one
conversation. `backlog add <dir|name>` saves the item in your thread, bound to
that directory. The TUI provides `/backlog`, `/backlog all`, `/attention`,
`/reply <id> <answer>`, `/backlog add`, `/backlog edit`, `/backlog run`,
`/backlog complete`, and `/backlog reconcile`. In the thread, `/backlog` narrows
to the focused project, and `/backlog add` uses the focused project or the
selected task's directory; with neither it asks which project and saves
nothing. Open a row for its complete prompt, summary, status and ID.
Mutations carry the displayed revision; a stale view cannot overwrite newer work.

The attention inbox spans agents and distinguishes questions, approvals, required
actions, and uncertain effects. A provider constraint that conflicts with an
automatic project proposal becomes a routing question. Reply with revised work
that respects the grant; xcb reroutes against fresh availability. A reply never
grants host permissions, repairs credentials, releases account custody, or answers
another approval automatically.

Every managed task is also its history entry. Its final report records changes,
checks and blockers. Failed and uncertain work retain their status. Completing a
deferred item records a reported summary; it does not pretend a worker ran.
Reconciliation changes uncertainty only when the exact retained underlying turn
has a conclusive outcome and all worker runs have settled. Missing process IDs or
elapsed time alone cannot prove arbitrary effects completed. If evidence is
missing, the item remains visible and blocks automatic progression.

## Steering and the durable inbox

```sh
xcb steer <task-id> "Keep the public API unchanged" --id <event-id>
xcb watch <target-task-id> <source-task-id> --id <watch-id>
xcb inbox --task <task-id> --json
xcb inbox --conversation <conversation-id> --limit 64 --json
```

Use `/steer <task-id> <text>`, `/watch <target-task-id> <source-task-id>`, and
`/inbox [all|task-id]` in the TUI. Explicit steering queues guidance for that task;
ordinary chat still creates work. The optional CLI `--id` lets a caller retry the
same operation without duplicating it. Reusing an ID with changed input fails.
Inbox queries are newest first. `--task` and `--conversation` are mutually
exclusive; `--before <sequence>` pages older results within the selected scope,
and `--limit` accepts 1–256 rows, defaulting to 64.

Acceptance means xcb persisted the event. Preparation means an exact set of
events was selected for a worker turn. Settled delivery means that set was
included in a proven worker turn; it does not prove the model understood or
followed the guidance. Events accepted after preparation remain pending for a
later turn. Reading the inbox or a worker's mailbox does not acknowledge prompt
delivery.

Guidance reaches a running task at its next safe turn boundary. It does not
interrupt a provider turn, answer an approval, release deferred work, reset an
attempt budget, or expand a project grant. Existing cancellation, attention,
project pause and expiry, provider constraints, and custody checks still apply.
Held events remain visible while a gate prevents another turn. Closed tasks and
tasks with immutable ALGAL program inputs reject new guidance. Resolve a question
or approval through its existing attention flow; steering cannot substitute for
that response.

A watch requests the source task's terminal report for the target task in the
same workspace, whichever conversation each task belongs to. A watch across two
different workspaces is refused. Its stable identity prevents duplicate reports when
registration or settlement is replayed. A waiting inbox entry reserves the report
until its source settles conclusively. A report arriving after the target closes
remains visible history and does not reopen the task. Explicit guidance and
subscribed reports may request another authorized turn, but available events
share a bounded batch instead of each creating its own dispatch. Overflow remains
pending; text omitted by the prompt limit is not marked delivered. Cancellation,
answers and approvals acquire no batching delay.

The inbox records context and delivery evidence, not additional execution
authority. Unknown worker settlement retains custody and prevents an automatic
retry. See [inbox measurement](inbox-measurement.md) for what event batching can
demonstrate and how to compare it without assuming token or cost savings.

## Bounded autonomy

```sh
xcb projects configure <dir|name> "Maintain and improve the parser" --tasks 10 --hours 24
xcb projects configure <dir|name> "Climb the migration" --tasks 100 --hours 720 --parallel 4 --per-hour 12
xcb projects scale <dir|name> --parallel 1 --per-hour 4 --revision 3
xcb projects status <dir|name>
xcb projects --json
xcb projects pause <dir|name> --revision 4
xcb projects resume <dir|name> --revision 5
```

In the TUI, use `/project grant 10 24 Maintain and improve the parser`, `/project`,
`/project all`, `/project pause`, and `/project resume`. Each takes an optional
project before its arguments: `/project grant [dir|name] <tasks> <hours> <goal>`.
After `grant`, a first word that is a whole number is `<tasks>`; write a
directory named only with digits as a path, such as `./2026`. Without a project
the command uses the project view's directory, then the thread's focus, then the
selected task's directory. It never guesses: in the thread with none of these it
says `name the project: /project grant <name|dir> …`.

`xcb projects` lists one row per directory with its name and status: `active`,
`paused`, `paused by upgrade`, `expired`, or `spent`. `--json` rows carry
`workspace`, `name`, and `status`, and `conversation` names the latest project
view over that directory, or is null. CLI replacement grants
require the current revision. A grant contains a user-authored goal, an expiry
from one hour to 30 days, and a budget of 1–10,000 automatic follow-up tasks. An
optional CLI `--provider` is a hard constraint inherited by automatic work.
Configuring a grant does not invent an initial task: submit the first prompt,
release a backlog item, or add a schedule to begin the project work.

Two throughput dials scale token spend on a long-running project without
editing it: `--parallel` (0–64) caps how many provider-bound tasks the project
may run at once, and `--per-hour` (0–512) caps how many automatic tasks it may
start per hour. `0` leaves the project's own limit unset; global capacity,
account, and directory rules still apply. `xcb projects scale` changes the
dials under the same revision check as every grant change, and
`xcb projects status` shows live workers, open and unfinished work, starts in
the current hour, and the schedules feeding the project.

The global `max_active_runs` setting is a ceiling of 1–64 tasks;
`max_runs_per_account` caps each subscription at 1–32 simultaneous runs.
With `adaptive_parallelism` enabled, the supervisor starts at one and reviews
capacity every 30 seconds. It adds at most one slot when the current target is
occupied, another independent workspace has ready work, and local account data
shows spare capacity with positive remaining quota. Paused, deferred, held,
recovering and waiting-for-input work does not justify growth. Account access,
provider/model support and workspace checks still run for each launch.

Host pressure halves the target. Host warnings prevent growth, and a shortage
of account capacity or ready work reduces it. Reducing the target lets running
work finish. Resource protection must be enabled and reporting successfully to
observe host pressure; when disabled, it supplies no host-health evidence.
Unknown quota cannot justify adding slots, though the normal router may still
attempt work at the current target. A restart begins at one again. The latest
local decision, timestamp, demand and reason are saved in
`managed/adaptive-capacity.json` under the state directory. This is an
observation, so check its timestamp when the supervisor is stopped.

These controls limit concurrency and starts, not tokens or dollars. Managed
accounts currently have no token-runway estimate. Priority then age determines
which ready task gets a slot; projects have caps, without weighted fair shares.
A sequential program stays sequential even if its project's cap increases.

For a month of unattended work, budget every child and leave recovery headroom.
An hourly program with five agent cells needs about 3,600 child tasks over 30
days, plus about 720 parent records. The explicit 10,000-task grant maximum
supports that budget; existing grants keep their original limits. Expiry stays
at most 30 days and is never renewed automatically. The shared store keeps up
to 65,536 task records, with separate limits of 128 unfinished tasks, 128
schedules, 200,000 messages and a 4 GiB database. Large outputs or many projects
can reach another bound first. Status and preflight estimates should inform the
budget; a large grant does not promise a month of available subscription usage.
Retention keeps its existing 30-day horizon and protects unresolved work and
dependencies. Increasing capacity does not delete history.

Routing and continuation rules can learn from local labeled outcomes, compare
candidates against fresh labels, and keep versioned parameters for rollback.
This does not yet optimize project progress per token or automatically roll
back a policy after a production regression. A month-long deployment still
needs an external, tested recovery copy and reviewable records of goals,
validation results and blockers. No schedule or grant is created by changing
these limits.

A grant authorizes automatic work in its own directory. It covers every
task bound there, from the thread, a project view, or a remote dispatch, and no
task in any other directory, even one in the same thread. An explicit binding to
a subdirectory such as `/repo/sub` is a different project from `/repo`; prompts
that mention a path inside a repository bind to the repository root, so the
root's grant applies to them. Linked worktrees of the same repository are the
one exception: they share the checkout's pause and throughput limits, so
automatic work across the whole family slows together rather than racing on
sibling checkouts. Your own prompts never wait for the dials — they count as
live work but are not rate-limited as automatic starts.

Workers propose deferred follow-ups with `xcb_backlog_add`. xcb admits one only
when its parent completed conclusively, the proposal belongs to the current
grant, no project work or attention remains in that directory, and the budget
and expiry allow it. The follow-up inherits its parent's directory.
Admission and budget consumption are atomic. User-added deferred items still
require release. Agents cannot expand their own grants or create schedules.
The goal guides reasoning; xcb enforces project boundaries, provenance, budgets,
and provider constraints rather than claiming to prove semantic task scope.

Pause holds future automatic dispatch and project schedule occurrences while
running work settles. Resume keeps the same consumed budget and expiry. A new
grant replaces the old generation; queued old automatic work does not acquire
new authority implicitly. Expiry and exhaustion stop automatic proposals.
Explicit schedules have their own recurring authority and remain bounded by
ordinary per-task attempt/time limits; pause them separately when ending them.

## Schedules and startup

```sh
xcb schedules
xcb schedules add <dir|name> "Inspect the project and report the next useful step" --every 3600
xcb schedules show <schedule-id>
xcb schedules edit <schedule-id> --revision 2 --every 7200 --next-in 300
xcb schedules pause <schedule-id> --revision 3
xcb schedules resume <schedule-id> --revision 4
xcb schedules delete <schedule-id> --revision 5
xcb schedules --workspace ~/src/app --enabled --due
```

Use `/schedule`, `/schedule all`, `/schedule every 3600 <prompt>`, and
`/schedule pause|resume <id>` in the TUI. A schedule is fixed to one directory
when you create it and never picks one when it fires. In the thread that
directory is the focused project or the selected task's directory; with neither,
xcb asks which project and saves nothing. From the CLI, `<dir|name>` puts the
schedule in your thread for that directory; a conversation ID keeps it in that
project view, and the thread's ID needs `--workspace <dir>`. The host owns the clock; no provider-native
scheduler is involved. Downtime coalesces missed intervals into one occurrence.
Durable occurrence identities prevent duplicate enqueue, and outstanding work,
questions or uncertainty block overlapping project occurrences.

Every mutation takes the schedule's current revision, so an edit and a wake-up
can never interleave into a half-applied change. The list view reports why a
due schedule is not running — a paused project, open work, an unfinished
previous run, or the project's hourly start limit — and its wake-up stays put
until the blocker clears instead of being silently skipped. `xcb native
status --json` carries the same schedule and project sections for agents.

The supervisor remains alive while enabled schedules exist. Closing the terminal
detaches; reopening xcb resumes persisted state. Opt-in [macOS login
startup](habitat-service.md) restarts the supervisor after desktop login. Without
a service manager, restart xcb after reboot. No task runs while the machine is
powered off. Persistent work is a sequence of accountable bounded tasks.

## ALGAL programs

```sh
xcb schedules program <dir|name> examples/project-planner.algal.json --inputs examples/project-planner-inputs.json --every 3600
```

Registration validates and pins the full manifest, typed input values and their
digests. Moving or editing the original file cannot change an existing schedule.
The program must expose a text `summary` and may expose a text `prompt`. Its
summary and receipt digest become ordinary task history; a prompt becomes a
deferred proposal and needs the same project grant as worker proposals.

Pure planners admit deterministic `input`, `const`, `fn` and `expr` cells with
bounded graph size, steps, work, context and output. Useful pure functions include
ALGAL memory queries and context compaction over explicitly supplied input data.
There are no imports, host effects, transports, or persistent VM store. Cancellation
joins bounded execution before settlement. Restart can replay pure work safely;
proposal publication retains stable occurrence identity.

Managed controllers request ordinary worker tasks and resume from their
completed reports. Enable managed calls with `--managed-calls`, from 1 to 8
calls per run:

```sh
xcb backlog program <dir|name> examples/project-controller.algal.json --managed-calls 2 --title "Inspect and advance the project"
xcb backlog program-status <parent-or-child-id> --json
xcb schedules program <dir|name> examples/project-controller.algal.json --managed-calls 2 --every 3600
```

The example inspects the project, then supplies that report to a second worker.
Managed manifests expose a text `summary` and admit top-level text-output `agent`
cells alongside pure cells. Provider routes, retries, imports, nested programs,
arbitrary host tools and subprocess backends are excluded. Inputs and manifest
bytes remain pinned. Each child uses normal model routing and project tools.

A current project grant is required. Immediate admission requires other released
project work to have settled; deferred backlog items may remain. Every child consumes
one grant task atomically when it is published and inherits the exact grant
generation and provider constraint. Pause, expiry, replacement grants, exhausted
budgets and unresolved attention hold further calls. Program children can record
deferred follow-ups, but cannot extend the controller's automatic work budget.

The controller releases its worker slot while waiting. Its checkpoint, receipt,
linked child and call identity survive restart, so resuming a recorded call does
not launch it again. Only the exact completed child's intact report can resume
evaluation. Failure or cancellation stops the controller; questions, approvals
and uncertain execution stay in the normal attention flow. The controller cannot
answer an approval. Reports and call prompts are bounded to 8 KiB; checkpoints
are bounded to 512 KiB. Oversized output is rejected rather than silently clipped.

Use `/program` in the TUI to browse recent controllers, or `/program <task-id>` to
inspect the parent, call count, linked child status and latest receipt. Open
`/attention` to resolve a child's question or approval. CLI inspection accepts
either a parent or child ID and includes retained history outside the bounded TUI
view. Cancel the parent through the ordinary task controls to cancel its active
child; xcb waits for settlement before reporting cancellation as complete.

No controller, schedule or provider is activated by an upgrade. These controls
extend the existing supervisor; they do not require another daemon.

## Working memory and Wordcell

`xcb backlog memory <dir|name>` and worker `xcb_memory_recent` return a
bounded newest-first set of task summaries with IDs and statuses from every
conversation over that directory, and never from another directory. Fresh workers
receive a small recent working set. This is historical context, not proof that a
file, dependency, account or service is unchanged. Terminal history has a bounded
30-day retention window; unresolved work and proposal/schedule dependencies remain
protected. Keep durable decisions in the external project vault.

```sh
xcb memory configure <dir|name> --vault /absolute/project/vault --wordcell /absolute/bin/wordcell
xcb memory status <dir|name>
xcb memory search <dir|name> "parser decision"
xcb memory promote <task-id> --body-file decision.md
```

The host explicitly binds a canonical local Wordcell vault to one project
directory and pins the executable and interpreter. `memory promote` writes to
the vault bound to the source task's directory. Replacing either requires rebinding with the current revision.
Workers use `xcb_memory_search`, and the TUI supports `/memory search <query>`.
Search uses Wordcell's exact local mode with bounded results, no history or graph
expansion. Retrieved records remain cited, untrusted context.

Promotion writes only the supplied short UTF-8 note plus source task provenance.
It never exports a conversation or copies private summaries automatically.
Deterministic note identity and Wordcell's no-clobber creation make identical
promotion idempotent. The harness retains an intent and process receipt before a
write; interrupted or unproven writes report uncertainty instead of success.
Retain applicability conditions and validation references in the authored note so
future agents can distinguish a past report from current evidence.

## Collaboration

`xcb_swarm_status`, `xcb_message_list`, and `xcb_message_send` provide durable
coordination between active tasks in the same workspace. Backlog and memory tools
are scoped to the worker task's own directory. Messages do not expand authority or expose other workspaces.
Inter-agent messages enter the target's durable inbox and share its bounded
delivery batches. Use an explicit watch when a task needs another task's terminal
report. Never wait synchronously for another task blocked on the same workspace
lock.

## Upgrading from an 0.8 release

Grants and Wordcell bindings belong to directories, while 0.8 releases kept
them on conversations. The first command of a newer build that opens 0.8 state
upgrades it once, after saving a copy as
`managed/managed.pre-v7.<time>.sqlite` in the state root when there is room.
That copy is the only way back: 0.8 refuses upgraded state, so rolling back
means stopping the supervisor, restoring the copy, and reinstalling 0.8.13,
which loses activity recorded after the upgrade.

1. Run `xcb doctor --upgrade-plan` with the new binary. It upgrades a private
   copy and lists what would move, pause, or unbind; your state is not changed.
2. Let running work finish, then quit every xcb terminal on the machine before
   installing. While an 0.8 supervisor is still running, the new build waits
   up to 20 seconds for it to exit and then refuses to open your state.
3. After installing, run `xcb workspaces conflicts`, then `xcb projects`.
4. Resume each grant listed as `paused by upgrade` with `xcb projects resume
   <dir> --revision <n>` once you have checked its goal and budget.
5. Run `xcb memory status <dir>` for each project with a Wordcell vault, and
   `xcb memory configure <dir>` where it reports conflicting bindings.
6. Hide directories you no longer use with `xcb workspaces hide <dir|name>`.

How the upgrade treats existing settings:

- A grant that was the only grant for its directory moves to that directory
  unchanged, with its goal, budget, expiry, provider, and use so far. It now
  covers every conversation over that directory, including remote dispatches
  and thread tasks bound there, not only the conversation it was created in.
- When several conversations over one directory had grants, one is kept: the
  only active one, otherwise the grant of the most recently used conversation.
  If two or more were active, the kept grant is paused (`paused by upgrade`)
  until you resume it. The others are recorded as superseded, and their queued
  automatic work waits in `xcb attention`. Budgets and goals are never added
  together.
- Identical Wordcell bindings for one directory merge. Different bindings bind
  nothing, and searches fail with `conflicting Wordcell bindings from upgrade;
  run xcb memory configure <dir>` until you choose one.
- Rows that cannot be read, or whose conversation had no usable directory, are
  dropped and listed by `xcb workspaces conflicts`.
- Configuring or resuming a grant, or binding memory, for a directory closes
  its open conflicts. A notice in the terminal stays up while any remain.
- A project view rooted at your home directory or a hidden directory inside it
  can no longer start tasks; use a project directory instead.

See [unattended operation](unattended-operations.md) for recovery, account health,
completion checks, and the limits of month-long operation.
