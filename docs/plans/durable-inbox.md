# Durable agent inbox — 0.5.0

This increment applies the host event lifecycle from the Unreal Agent assessment
without changing provider protocols. A managed task remains the unit of work and
authority. The host records guidance and notifications, then includes an exact,
bounded batch in the next authorized provider turn.

## Contract

- `xcb steer <task> <text> [--id <event>]` and `/steer` durably queue explicit
  guidance. Ordinary chat still creates work. Guidance never answers an approval,
  interrupts a provider turn, releases deferred work, or resets an attempt budget.
  Closed tasks and immutable ALGAL program inputs reject new guidance.
- `xcb inbox` and `/inbox [all|task]` show persisted acceptance and delivery.
  Queued, prepared, settled delivery and held/closed states are distinct. Delivery
  means inclusion in a proven worker turn, never proof the model followed it.
- `xcb watch <target> <source> [--id <watch>]` and `/watch` explicitly request a
  terminal report from another task in the same project/workspace. Registration
  and settlement are idempotent. This does not resurrect closed targets. A report
  arriving after closure remains visible history. Existing inter-agent messages
  enter the same inbox; reading a mailbox does not acknowledge prompt delivery.
- Pending explicit guidance or a subscribed report can request the next safe
  turn of a running task, subject to the existing attempt, project, provider,
  cancellation, attention and custody gates. Notifications confer no new grant.
  Available events share one bounded batch, rather than one dispatch per event.
  No artificial delay is added to cancellation, answers or approvals.
- Freeze exact event membership before building the prompt; prepare and settlement
  persist that membership atomically with the task transition. Late arrivals stay
  pending. Never acknowledge clipped context. Restart and failover must distinguish
  unadmitted input, proven insertion, and uncertain outcomes.
- Additive schema 4 migration requires supervisor custody. Bound event bodies,
  rows, batch count and encoded bytes; preserve required evidence during retention.

## Shared interfaces and ownership

Core UI adds `InboxRow { id, task, conversation, sequence, kind, text, status,
created_at_ms, updated_at_ms, receipt: Option<String> }`, `View.inbox`,
`HabitatCommand::Steer { task, event, text }`, and
`HabitatCommand::WatchTask { task, source, event }`.

Runtime exposes serializable `InboxEvent`, `InboxWatch`, synchronous
`ManagedStore::steer_task(&Id, Id, String)`, `watch_task(&Id, &Id, Id)`, and
`inbox(task: Option<&Id>, conversation: Option<&Id>, before: Option<u64>, limit:
usize)` returning newest-first bounded events. Event data includes the core UI
fields; runtime owns precise state and receipt representation.

1. Contract convergence: this plan (root).
2. Parallel implementation in the existing checkout:
   - `inbox_runtime_spike`: all runtime files, migration, atomic event lifecycle,
     supervisor integration, runtime UI adapter and focused runtime tests.
   - `inbox_ui_spike`: core UI and TUI commands, inspection, rendering and tests.
   - root: CLI controls, release version, integration and delivery.
   - `inbox_measure_spike`: documentation and bounded measurement methodology;
     independent read-only review of runtime once available.
3. Join: compile interfaces, exercise operator commands and delivery races.
4. Independent review and required aggregate gates on the converged tree.
5. Current-head PR checks, merge, immutable release, actual artifact verification,
   publication datum/site verification and admitted local installation.

## Acceptance and measurement

Tests cover duplicate identity and changed payloads, simultaneous completions,
bounded overflow, late steering, restart at admission/settlement boundaries,
unknown custody, cancellation, attention, project pause/expiry and budgets,
mailbox source authentication, migration and retention. Visible-only inbox changes
must refresh the TUI. CLI/TUI must display held guidance without implying approval.

Measure unique events, batches, events per batch and host provider attempts.
Provider-reported usage is deduplicated by observation identity. Missing usage,
inner model-request counts and billing remain unknown. A deterministic batching
test demonstrates coalescing, not live token or dollar savings.

Required final gates: workspace tests, strict all-target Clippy, formatting,
compatibility `bun run check`, site `bun run check`, operator smoke and independent
review. Publication and activation remain separate; installation does not enable
services, schedules or unqualified providers.

## Status

Implementation and independent feature review are complete. Native workspace
tests, strict Clippy and the site gate passed before integrating Reflexes v2.
Final integration is checking continuation vetoes and learning attribution with
that update, followed by fresh aggregate gates and operator acceptance. Release,
publication and installation remain pending; no v0.5.0 publication is claimed.
