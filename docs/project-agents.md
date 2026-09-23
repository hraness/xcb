# Persistent project agents

A managed conversation is a persistent project agent. Open it again by its
conversation ID to find the same project, backlog and history. Each ready task
can use a different eligible provider/model; the conversation remains stable.

## Work and attention

```sh
xcb backlog --conversation <conversation-id>
xcb backlog add <conversation-id> "Review the next milestone" --priority 7
xcb backlog edit <task-id> "Review the authentication milestone" --revision 1
xcb backlog release <task-id> --revision 2
xcb attention
xcb backlog reply <task-id> "Use the existing project conventions"
xcb backlog memory <conversation-id>
xcb schedules add <conversation-id> "Inspect the project and report the next useful step" --every 3600
xcb schedules pause <schedule-id> --revision 1
```

The TUI supports `/backlog`, `/backlog all`, `/attention`, and `/schedule`.
Use `/schedule every 3600 <prompt>` for an hourly prompt, then inspect its row
for its ID and enabled state. Backlog edits and releases use the revision in
the displayed row so a stale view cannot silently overwrite another edit.

A backlog item can be held for later or released for execution. Deferred items
are editable with revision checks. Running work preserves its original goal and
receipts; a follow-up is explicit additional input. Priority orders ready work,
and a workspace still admits only one coding turn at a time.

The attention inbox spans conversations. Questions, approvals and actions retain
their distinct states. Open an item to see its detail and answer in its owning
conversation. Answering a question does not prove an external approval, repair
credentials, release an uncertain account lease or bypass a provider boundary.

Every managed task is also its work-history entry. Its settled final response is
the summary; failed and uncertain work keep their status instead of appearing
completed. Workers are instructed to summarize changes, checks and blockers.
There is no second synthetic completion task to keep in sync.

The existing managed-store retention policy keeps terminal task history for
30 days within bounded storage. Nonterminal backlog work and unresolved uncertain
work are retained; schedules retain the last task needed to prove whether
recurrence can proceed. Recent
working memory follows retained task history. Promote durable findings to
Wordcell through the project integration when they should outlive that window.

## Recurring work

An explicitly created interval schedule stores the prompt, conversation, next
due time and enabled state. xcb's local supervisor supplies wall-clock wakeups.
The provider receives an ordinary bounded managed task. The daemon stays alive
while enabled schedules exist; closing a terminal detaches from it.

Downtime coalesces missed intervals into one occurrence. An occurrence identity
makes enqueue replay safe across restarts. A schedule does not overlap its own
unfinished work. A task needing human input blocks recurrence until answered.
An uncertain task blocks that conversation's schedules indefinitely: this
version has no in-place managed-task uncertainty resolution operation. Inspect
and recover the underlying run and effects before explicitly creating a new
conversation and schedule; opening another conversation does not release any
account or workspace custody. Pausing future occurrences
does not cancel a running task. The supervisor is local: starting xcb again is
required after the machine's process manager terminates it or after reboot.
No OS login service or provider-native timer is installed by these commands.

Schedules retain ordinary task attempt/time limits. Persistent operation means
many separately accountable bounded tasks, not an unbounded continuation loop.

## Agent tools and collaboration

Managed workers can inspect their project's backlog with `xcb_backlog_list`,
read a complete current prompt with `xcb_backlog_get`, propose deferred work
with `xcb_backlog_add`, and edit deferred unstarted work with
`xcb_backlog_update`. Stable call identities and expected revisions protect
replay and concurrent edits. These tools do not release tasks or create timers.
The user supplies execution authority through the control conversation or CLI.
Agent-authored follow-ups therefore do not form a self-draining queue yet.

`xcb_swarm_status`, `xcb_message_list` and `xcb_message_send` retain durable
coordination between active tasks in the same workspace. Cross-workspace access
requires an explicit host authorization design; a task cannot discover unrelated
projects or request a broader capability by sending a message. Never wait
synchronously for another task blocked on the same workspace lock.

## Working memory and Wordcell

`xcb_memory_recent` returns a bounded newest-first working set of recent settled
summaries, with task IDs and statuses. Fresh worker contexts can use this local
working set before retrieving external knowledge. A historical report says what
a previous agent concluded; it is not proof that a file, dependency, account,
service or policy is unchanged.

Use ALGAL's memory layer for observations with explicit applicability
prerequisites and derivation provenance. Its bounded fact/query results can be
cached locally, but changed dependencies require new evidence. Retain the source
record and receipt when exporting an insight rather than turning model output
into an observed fact.

Wordcell is the external long-term project knowledge system. Keep durable
project decisions, reusable procedures and verified findings there through the
project's existing integration. The local work history remains available without
Wordcell connectivity. This change does not upload transcripts, synchronize
private summaries automatically, overwrite external memory, or claim cached
knowledge is current. A future explicit promotion operation should record the
source task, workspace, validation evidence, applicability dependencies and the
Wordcell reference; fetching should retain source timestamps and provenance.

## ALGAL programs

The existing `algal.process.v1` process stores manifests, generations, mailbox
wakes and receipts. Its `schedule` operation subscribes to mailbox events; it
is not a wall-clock timer. xcb is the right owner for real time and process
liveness. A program-backed schedule should dispatch a fresh bounded invocation
of a pinned admitted manifest, retaining its ordinary host capabilities, fuel,
process-generation limits and effect receipts. Scheduling must never turn a
model-supplied path or shell command into a trusted host executor.

The initial timer surface dispatches prompts. Program registration and execution
remain a separate host admission boundary; the VM does not need an infinite-loop
mode to support a persistent project agent.

## Next integration boundaries

Fully autonomous backlog execution needs a conversation-level policy defining
the authorized project goal, permitted task sources, concurrency, cost/attempt
budgets and pause conditions. A worker proposal should become runnable only
after the host admits it against that policy. Human questions and provider or
host approvals remain distinct attention items; a schedule must not answer them.

Program schedules should store an admitted manifest digest and typed inputs,
not an executable path. Each occurrence should produce a normal task identity,
VM receipt and work summary, with cancellation joining all host effects before
settlement. A restart should reconcile the occurrence receipt before dispatch.

Router-specific clarification is also future work. Today the router applies
explicit provider constraints and estimates demand; worker questions surface
through the attention inbox. A future route question should carry bounded
choices and the constraint that requires clarification, then reroute against
fresh availability after the user answers. Routine model selection should
remain automatic.
