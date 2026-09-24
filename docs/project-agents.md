# Persistent project agents

A managed conversation is a persistent project agent. Reopen its conversation ID
for the same workspace, backlog, work history, and goal. Each task routes to an
eligible model automatically; large prompts prefer known frontier quality, and
observed usage limits produce a warning when they force a lower-ranked route.
Model selection normally needs no input from you.

## Work, questions and approvals

```sh
xcb backlog --conversation <conversation-id>
xcb backlog add <conversation-id> "Review the next milestone" --priority 7
xcb backlog edit <task-id> "Review authentication" --revision 1
xcb backlog release <task-id> --revision 2
xcb backlog complete <deferred-task-id> "Already covered by the passing parser tests" --revision 1
xcb attention
xcb backlog reply <task-id> "Use the existing project conventions"
xcb backlog reconcile <uncertain-task-id> --revision 4
```

The TUI provides `/backlog`, `/backlog all`, `/attention`, `/reply <id> <answer>`,
`/backlog add`, `/backlog edit`, `/backlog run`, `/backlog complete`, and
`/backlog reconcile`. Open a row for its complete prompt, summary, status and ID.
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
same project and workspace. Its stable identity prevents duplicate reports when
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

The additive upgrade preserves the existing mailbox and imports messages for
open recipients with stable event IDs. Earlier versions did not record prompt
consumption, so a previously read message may appear in a worker prompt once
after upgrade. Imported messages have no invented delivery receipt and still
respect deferred work, attention and custody gates. Migration waits for exclusive
supervisor custody; provider adapters and their qualification remain unchanged.

## Bounded autonomy

```sh
xcb projects configure <conversation-id> "Maintain and improve the parser" --tasks 10 --hours 24
xcb projects --json
xcb projects pause <conversation-id> --revision 1
xcb projects resume <conversation-id> --revision 2
```

In the TUI, use `/project grant 10 24 Maintain and improve the parser`, `/project`,
`/project all`, `/project pause`, and `/project resume`. CLI replacement grants
require the current revision. A grant contains a user-authored goal, an expiry
from one hour to 30 days, and a budget of 1–100 automatic follow-up tasks. An
optional CLI `--provider` is a hard constraint inherited by automatic work.
Configuring a grant does not invent an initial task: submit the first prompt,
release a backlog item, or add a schedule to begin the project work.

Workers propose deferred follow-ups with `xcb_backlog_add`. xcb admits one only
when its parent completed conclusively, the proposal belongs to the current
grant, no project work or attention remains, and the budget and expiry allow it.
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
xcb schedules add <conversation-id> "Inspect the project and report the next useful step" --every 3600
xcb schedules pause <schedule-id> --revision 1
xcb schedules resume <schedule-id> --revision 2
```

Use `/schedule`, `/schedule all`, `/schedule every 3600 <prompt>`, and
`/schedule pause|resume <id>` in the TUI. The host owns the clock; no provider-native
scheduler is involved. Downtime coalesces missed intervals into one occurrence.
Durable occurrence identities prevent duplicate enqueue, and outstanding work,
questions or uncertainty block overlapping project occurrences.

The supervisor remains alive while enabled schedules exist. Closing the terminal
detaches; reopening xcb resumes persisted state. Opt-in [macOS login
startup](habitat-service.md) restarts the supervisor after desktop login. Without
a service manager, restart xcb after reboot. No task runs while the machine is
powered off. Persistent work is a sequence of accountable bounded tasks.

## ALGAL programs

```sh
xcb schedules program <conversation-id> examples/project-planner.algal.json --inputs examples/project-planner-inputs.json --every 3600
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

Current source builds also support managed controllers that request ordinary
worker tasks and resume from their completed reports. This profile is not in
the published v0.5.0 binaries. Explicitly enable it with `--managed-calls`, bounded
from 1 to 8 calls per run:

```sh
xcb backlog program <conversation-id> examples/project-controller.algal.json --managed-calls 2 --title "Inspect and advance the project"
xcb backlog program-status <parent-or-child-id> --json
xcb schedules program <conversation-id> examples/project-controller.algal.json --managed-calls 2 --every 3600
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

`xcb backlog memory <conversation-id>` and worker `xcb_memory_recent` return a
bounded newest-first set of task summaries with IDs and statuses. Fresh workers
receive a small recent working set. This is historical context, not proof that a
file, dependency, account or service is unchanged. Terminal history has a bounded
30-day retention window; unresolved work and proposal/schedule dependencies remain
protected. Keep durable decisions in the external project vault.

```sh
xcb memory configure <conversation-id> --vault /absolute/project/vault --wordcell /absolute/bin/wordcell
xcb memory status <conversation-id>
xcb memory search <conversation-id> "parser decision"
xcb memory promote <task-id> --body-file decision.md
```

The host explicitly binds a canonical local Wordcell vault and pins the executable
and interpreter. Replacing either requires rebinding with the current revision.
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
coordination between active tasks in the same workspace. Backlog tools are scoped
to the owning project. Messages do not expand authority or expose other workspaces.
Inter-agent messages enter the target's durable inbox and share its bounded
delivery batches. Use an explicit watch when a task needs another task's terminal
report. Never wait synchronously for another task blocked on the same workspace
lock.
