# Managed harness

Native XCB separates a user's control conversations from provider worker
sessions. Each terminal has its own transcript and draft. Conversations share
durable tasks; each task retains its originating conversation, workspace,
original goal, explicit follow-ups, worker history, and transition receipts.

## Responsibilities

| Component | Responsibility |
| --- | --- |
| Managed store | Atomic intake, task revisions, replies, mailbox delivery, receipt history |
| Supervisor | Dispatch, bounded continuation, cancellation, restart reconciliation |
| Router | Rank already eligible account/model routes using explicit heuristics |
| Kernel and runner | Workspace/account custody, provider admission, effects, settlement |
| ALGAL | Deterministic, replayable recording of bounded transition records |
| Optional judge | Rank eligible routes or evaluate continuation after safety gates |

The pinned ALGAL program records its input. Rust enforces the state machine,
admission and custody contracts. Receipt replay proves consistency of these
local records; it does not prove task correctness, provider attestation, or
that an external effect occurred. This implementation does not execute
self-modifying orchestration policies.

## Task lifecycle

Intake atomically stores the user message, task, acknowledgement and initial
receipt. Reusing a message ID with different input is rejected. A supervisor
prepares one provider session, then executes one turn; managed tasks do not run
the direct-session continuation loop beneath the supervisor.

After settlement the supervisor records one of:

- `completed`: a settled, completed provider turn. Checks remain worker-reported.
- `needs_input`: a worker question or an exhausted automatic dispatch budget.
- `queued`: a permitted continuation or checkpointed quota failover.
- `cancelled`: confirmed cancellation or cancellation before dispatch.
- `failed`: a definite failure without completion.
- `uncertain`: insufficient process/effect or terminal evidence. No automatic retry.

Automatic attempts are bounded. Explicit user input renews the attempt/time
budget and route exclusions. Original instructions, question/answer context,
and retained images accompany a new provider route. Exhausted history or
context limits fail the individual task visibly; they do not authorize
truncating the user's goal or retrying without bound.

Worker outcomes are recorded atomically with native run settlement. Restart
reconciliation matches the exact input sequence, session revision and
transcript boundary. An idle session alone cannot distinguish completion from
a turn limit. Legacy runs lacking terminal evidence remain uncertain.

Host execution errors and recognized provider errors retain a bounded,
host-selected diagnostic with their native outcome. Managed task details show
it after settlement or restart. Raw provider errors, stderr, credentials and
operating-system paths are excluded.
Diagnostics explain failures; they do not authorize retries or release custody.

## Concurrency and effects

One detached supervisor owns a state root. Native execution also enforces
account custody and workspace exclusion, including direct sessions and other
terminals. Independent workspaces can run concurrently. Tasks in the same
workspace execute serially; a worker must not wait synchronously for a queued
peer that cannot acquire that workspace.

A per-task dispatch or settlement fault is isolated to that task, recorded
as a bounded detail, and retried with backoff; it does not stop the other
workers. Lock, identity and store failures stay fatal, as does a sustained
run of ticks that cannot even list tasks. The last supervisor-level fault
is kept in a bounded file the next client surfaces. Task and session list
readers skip a corrupt row rather than fail the page; single-row reads and
transitions stay strict, and skipped task rows are counted for the view.

Cancellation belongs to the originating conversation unless the user names a
task. Closing a terminal detaches. Active managed worker sessions are protected
from session removal/pruning. A replacement supervisor binary drains settled
workers before retiring; clients reject an incompatible or unidentified owner
instead of silently reusing it or signalling an unverified PID.

Worker mailbox tools expose active tasks only within the same workspace.
Delivery rechecks source session/turn and target state within a transaction.
A stable call identity can replay an identical delivery but cannot change its
body or recipient. Messages persist across provider handoff; their content
does not widen task authority. The injected inbox is bounded and the complete
mailbox remains available through paginated `xcb_message_list`.

## Routing and offers

Provider/runtime admission, credentials, account availability and route
exclusions are applied before shortlisting and ranking models. Explicit
provider directives constrain both dispatch and route preview. A learned
workspace preference is a soft ranking input. Relative quality, cost and
latency values are heuristics, not measured quality or billing guarantees.
When no admitted, enabled, credentialed account exists, a queued task says
so and waits for the user to add or reconnect one; a temporary route
shortage retries with backoff.

Public promotions are bounded, expiring observations with source digests.
They do not prove a user's entitlement and cannot qualify an unadmitted
provider. Failed offer refresh does not block task dispatch.

## Verification

`xcb tasks verify <task-id>` replays the ALGAL receipt chain and compares it to
the persisted task and its immutable identity. `xcb tasks messages <task-id>`
inspects the durable mailbox. `xcb models route --task TEXT` uses the managed
intake provider preference and workspace context.

The regression suites exercise all three provider protocols synthetically,
mailbox idempotency and scope, quota/cancellation boundaries, transcript and
draft isolation, exact restart evidence, receipt corruption, session retention,
and supervisor replacement. Live coding acceptance, command-runner admission,
installation and artifact publication are separate evidence. Passing the
synthetic suites does not establish a live cross-provider handoff.
