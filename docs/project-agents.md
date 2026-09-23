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

This release admits deterministic `input`, `const`, `fn` and `expr` cells with
bounded graph size, steps, work, context and output. Useful pure functions include
ALGAL memory queries and context compaction over explicitly supplied input data.
There are no imports, host effects, transports, or persistent VM store. Cancellation
joins bounded execution before settlement. Restart can replay pure work safely;
proposal publication retains stable occurrence identity.

Effectful programs need a resumable xcb host adapter before admission. The pinned
VM's generic subprocess backend cannot carry managed parent identity, private
state-root, tool custody and approvals, so it is rejected. Providers and tools
continue through normal xcb tasks produced by the planner.

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
Never wait synchronously for another task blocked on the same workspace lock.
