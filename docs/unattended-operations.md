# Unattended operation

An unattended project needs an explicit objective, a durable work queue, enough
account capacity, and evidence that its requested delivery actually finished.
A successful model response is only one part of that contract.

## Recovery and account health

`xcb native status --json` reads local records without contacting a provider.
Each account's `health` separates authentication, quota, provider availability,
and held execution state. A recent successful inference can support `healthy`;
missing or old inference evidence stays `unknown`. Metadata refresh alone does
not prove inference access. `quota_wait`, `provider_backoff`, and
`recovery_trial_due` include their recorded reset or retry times. A held idle
account needs its process and effect records inspected before reuse.

Explicit, settled provider-capacity refusals use persistent exponential backoff
with jitter. Delays start between 15 and 30 seconds and grow to between 15 and
30 minutes. Account cooldowns are shared across tasks; after a cooldown, only
one trial can run until a settled success clears the failure streak. Rotated
credentials do not inherit another generation's health record.

Task recovery has a separate 35-day horizon and a 4,096-refusal limit. It never
extends a project grant or replaces its task budget. Positively settled attempts
with no effects do not consume the productive-turn budget. Temporary quota
exclusions expire after the observed reset or configured fallback cooldown;
all accounts being exhausted therefore means waiting, not permanent exclusion.

Automatic retry needs conclusive completion and process cleanup. A disconnected
stream, unproven command result, authentication refusal, or permission denial
is not evidence that replay is safe. Claude quota observations are supported;
its generic error responses are not guessed to be transient outages. Unsupported
failure categories remain visible for diagnosis rather than being replayed.

## Capacity and month-long budgets

See [project controls](project-agents.md) for the exact concurrency algorithm,
explicit budgets, limits, and examples. Capacity grows by one slot per interval
only when existing capacity is occupied, independent work is ready, and account
capacity supports growth. Blocked work is not useful demand. Host pressure lowers
the target without killing existing work. User limits remain ceilings.

The advisory decision record is `managed/adaptive-capacity.json` under the xcb
state directory. Its timestamp matters: a stopped supervisor leaves an old
record. These controls regulate task starts and concurrency; they do not enforce
a token budget, predict subscription runway, or guarantee fairness across projects.

For a 30-day plan, count controller and worker records, child admissions,
verification work, expected retries, and retained history. Reserve physical disk
headroom for peak builds and logs. A larger task budget does not create account
capacity, and a single sequential controller does not become parallel merely
because its concurrency ceiling increases.

## Completion and confirmations

The project objective and delivery instructions define completion. A request for
a draft or review does not authorize merging it. A request to ship requires the
applicable checks, merge, release, and production evidence; a response saying
that those steps remain pending must retain the remaining work.

The completion herder uses the original request and latest report to continue
required work within current project authority. Routine requests to proceed do
not need the owner to be present. A waiting external operation needs a later
check against the exact repository, PR head, workflow, or deployment identity.
A worker can request an exact-head PR watch with `WAIT_PR`. When its native
provider and workspace already have GitHub access, the host polls fixed
read-only PR and check endpoints with backoff. This consumes no provider turns.
A changed head, merged or closed PR, or settled checks wakes the worker to
reconcile delivery. The observer never merges and does not replace the
repository's readiness gate. Productive worker turns keep their own limits.

A worker's claim is not remote-state verification. Missing credentials, new
permissions, and uncertain effects cannot be repaired by inventing consent.

The optional judge supplies bounded classification and continuation advice.
See [API judge setup](chat-judge.md) for xAI, Vercel AI Gateway, and custom
endpoints. Its answers do not grant filesystem, credential, provider, or
publication permissions. Deterministic checks still govern each action. Use synthetic input
when testing a judge connection; never include provider secrets in its context.

## Learning, upgrades, and recovery drills

xcb's reflex learner fits candidates and compares them with fresh labels before
promotion. Previous parameter generations remain available for rollback. This
is limited decision tuning, not proof that arbitrary project changes improve
quality. Project experiments still need a fixed evaluator, a baseline, cost and
outcome records, and a keep/reject decision that the candidate cannot rewrite.

Native qualification binds the executable actually tested. Provider or xcb
updates can invalidate it, and receipts expire. Pin the validated deployment
for an unattended run or provide a tested, explicitly authorized requalification
procedure; never weaken the qualification check to keep work moving. A
provider-level fixture does not establish every account's subscription health.
`xcb native revoke` removes the selected workspace grant, including when its
qualification is stale, without terminating commands already in flight.

Before enabling a project for a month, test restart during a quota wait,
provider recovery, cancellation with a running command, an interrupted database
write, backup restoration into an isolated state root, and delayed delivery.
Preserve credentials and process/effect receipts during restore. Backups need a
consistent SQLite snapshot and a protected credential recovery path; copying a
live database file without its transaction state is not a recovery plan.

Offline tests and a short provider smoke test do not establish a month of live
reliability. Start with a bounded project, inspect its completed delivery and
recovery evidence, then expand its authorized budget. Installing a release does
not itself authorize or create a project schedule.
