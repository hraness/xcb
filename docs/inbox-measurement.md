# Measuring the durable inbox

The inbox records explicit steering, inter-agent messages and subscribed terminal
reports, then delivers available events together at an authorized turn boundary.
This can demonstrate fewer delivery batches than events. It does not by itself
demonstrate fewer provider-internal model requests, lower token usage, lower
billing, or better task outcomes.

The host already collected completed workers without starting a separate model
request for every completion. Compare the new behavior against that actual
baseline. A synthetic experiment that dispatches once per event is a policy
comparison, not evidence about an older xcb release.

## Evidence and definitions

| Measurement | Evidence and interpretation |
| --- | --- |
| Accepted events | Count unique durable event IDs. Retried identical submissions count once. Separate reserved watch entries from completion reports already available. |
| Waiting, pending, prepared, delivered, held or closed events | Report their distinct persisted states; a waiting watch has no conclusive report yet. Do not treat every accepted event as delivered. |
| Delivery batches | Count unique batch receipts, separating preparation from proven delivery. A restart or repeated observation does not create a new batch. |
| Events per delivered batch | Unique delivered event IDs divided by unique delivered batches. Also report the distribution and overflow still pending. |
| Acceptance latency | Client monotonic elapsed time from command submission to the successful persisted-acceptance response. |
| Delivery latency | Event acceptance to its proven delivery receipt. Report the observation basis and leave undelivered events censored, with their current wait and gate. |
| Host provider attempts | Count exact admitted host run identities, distinguishing unstarted dispatch, started work, settled outcomes and uncertainty. These are not exact provider-internal model requests. |
| Observed usage | Sum the latest record for each unique usage observation ID; retain uncached input, cache reads, cache writes and output separately. |
| Context size | Encoded prompt bytes or explicitly labeled estimated tokens. Context projection currently estimates text tokens as bytes divided by four, rounded up. |
| Tools and retries | Count unique admitted tool calls and host attempts, with their settlement states. Provider-internal retries are unknown unless exposed. |
| Task outcome | Apply the same acceptance checks to each run and record passed, failed, blocked or uncertain. |
| Billed cost | Report only provider billing evidence, when available; otherwise unknown. Subscription quota percentages are not dollar prices. |

Delivery is evidence of inclusion in a proven worker turn. It is not a semantic
acknowledgement that a model applied the guidance. Label any human or test-based
assessment of applied guidance separately. If the provider starts or settles
between host observations, a latency based on receipt recording includes that
observation delay; do not present it as exact provider-read time. Cross-process
wall-clock timestamps can move backward. Flag invalid intervals rather than
silently converting them to zero.

A watch reserves its event before the source report exists. Registration-to-
delivery latency therefore includes the source's work. Report that interval
separately from report-ready-to-delivery latency, and leave the latter unknown
unless the experiment retained the actual availability transition. A row's latest
update time is not a substitute for every earlier transition timestamp.

Upgrade migration imports existing mailbox messages for open recipients without
claiming prior delivery. Separate those imported events from newly accepted
experiment inputs: earlier versions had no consumption cursor, and a message may
be presented again once after migration. Stable imported IDs prevent duplicate
rows; they cannot reconstruct missing historical delivery times.

Native usage observations are monotonic upserts keyed by `UsageObservation.id` in
`store.rs`. A later snapshot replaces an earlier snapshot of that ID; it is not
additional usage. The runner persists provider-reported counters when available.
Missing observations mean unknown usage, not zero. Reasoning tokens, when present,
are a subset of output and must not be added to output again. The number of usage
records is not a model-request count: one host turn can report more than one
model's usage.

Local `xcb sessions export` writes bounded usage observations with stable hashed
identities. It does not publish them. The export intentionally omits exact model
identity, so retain the selected provider, model and effort in private experiment
metadata. Exports are bounded to the latest 2,048 observations; use small isolated
runs and check coverage instead of assuming an export contains every historical
run. Keep user prompts and event bodies private when sharing numeric results.

## A bounded experiment

Run the isolated operator acceptance script against the exact native candidate:

```sh
python3 qualification/inbox-acceptance.py /absolute/path/to/xcb --evidence-dir /existing/private/evidence-parent
```

On a host with the HRA scheduler, run that command through its installed absolute
`hra-host-run` path and the appropriate compute lane. The script creates a fresh
private state, home, workspace and coordination root, launches only its owned
daemon, and retains JSON, terminal captures and diagnostics. A future fixture
schedule keeps that daemon alive during the checks and is paused before shutdown.
No accounts or provider sessions are imported or activated. Timeout or an unjoined
daemon is a failed result, not a successful smoke check.

This script proves CLI/TUI routing, storage, replay, filtering, deferred holds and
late-report behavior. It does not prove provider delivery, batch settlement or
token savings. Deterministic runtime fixtures own those lifecycle assertions.

Start with a deterministic fake provider and isolated task/state fixtures. No
provider account or live credentials are needed to establish batch correctness.
Keep the exact source revision, workload, event IDs, task authority, batch bounds,
and expected outputs with the results.

1. Hold an authorized target at the fixture's dispatch barrier. Accept three
   short steering events and settle two explicitly watched sources, respecting
   workspace serialization. Release target dispatch only after all five events
   are available. Verify one bounded batch, preserving order and exact membership.
2. Accept another event after preparation, while the target runs. Verify that it
   is not acknowledged by the first batch and is eligible only for a later
   authorized turn.
3. Exceed either the event-count or encoded-byte bound. Verify ordered overflow
   and no acknowledgement of omitted context. Repeat near the total prompt limit.
4. Replay an accepted ID, then retry that ID with changed input. Verify one event
   for the identical replay and a conflict for the changed input.
5. Restart at the preparation and settlement boundaries. Verify stable batch
   identity, exact delivery recovery, and no replay of uncertain effects.
6. Repeat with cancellation, attention, exhausted attempt budget, paused or expired
   project authority, and a target that closes before a watched report arrives.
   Verify prompt delivery respects each gate and late reports remain history.

If the target can complete before all source events arrive, use a barrier in the
test fixture. A timeout is a failure to establish the precondition, not permission
to infer that an event arrived. Do not weaken project, provider or custody gates
to make the workload run.

For operator inspection, capture bounded JSON snapshots using the task ID:

```sh
xcb inbox --task <task-id> --limit 64 --json
xcb inbox --task <task-id> --before <oldest-sequence> --limit 64 --json
xcb sessions export
```

An inbox page is newest first. Continue with the oldest sequence as `--before`
until the small experiment's known event set is covered. Merge snapshots by
stable event identity, keeping the latest state. Do not sum repeated pages or
assume that a status snapshot exposes every underlying dispatch or timing fact.
Use the runtime test's exact receipts for assertions that are not present in
operator output.

Only after correctness passes should a live comparison use the same qualified
provider, account plan, model and effort, task prompt, initial workspace snapshot,
tool permissions, project grant, and acceptance checks. Freeze event content and
arrival conditions, repeat each policy several times, and alternate run order.
Record warm/cold cache conditions, route changes, quota interruptions and missing
usage. Report per-run values plus median and range; do not hide failed or censored
runs in an average. If comparing against a different xcb version, retain its exact
revision and document every other behavior change that could affect the result.

## Release judgment

Duplicate delivery, false acknowledgement, dropped overflow, a gate bypass, or
automatic replay after uncertain settlement is a correctness failure regardless
of throughput. Cancellation and approval handling must not wait for ordinary
completion batching. An event still held by an authority or attention gate is not
a batching-latency regression; report the gate and wait separately.

Passing deterministic tests supports claims about durability, batching and
recovery. A provider-observed token change supports only that measured workload
and configuration. Dollar savings require billing evidence, and outcome quality
requires the same task acceptance checks. Until those observations exist, report
live token savings, inner model-request counts and cost savings as unmeasured.
