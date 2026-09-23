# Project agent release 0.4.0

The persistent habitat foundation is merged in PR 139. This release makes it
usable for delegated project work and publishes the first verified native xcb
artifacts. A managed conversation remains the project identity; its tasks remain
the queue, attention inbox, and work history.

## Scope and decisions

- Automatic routing remains the default, using the existing ALGAL classifier and
  observed eligible model catalog. Large prompts prefer frontier quality. Known
  quota fallback produces a warning. Provider constraints remain hard constraints.
- A user grants a project a goal, expiry, and finite number of automatic follow-up
  tasks. Only proposals from completed work in that grant can be admitted. Pause,
  expiry, exhaustion, questions, approvals, and uncertain effects stop progression.
  The goal is authoritative instruction, not a claimed semantic sandbox.
- Deferred backlog work can be edited or marked completed with a summary. Exact
  retained settlement evidence can reconcile uncertainty; elapsed time or absent
  processes alone never proves arbitrary effects completed.
- Schedules support pinned, bounded deterministic ALGAL planning programs. Their
  outputs and receipts belong in ordinary work history; generated work goes through
  ordinary project admission. Effectful ALGAL backends need a future resumable xcb
  effect adapter, since the pinned VM's subprocess backend cannot preserve xcb's
  parent task, state-root, and approval custody.
- Recent task summaries are local working memory. A project may explicitly bind a
  local Wordcell vault for bounded cited retrieval and explicit note promotion.
  No transcript export, automatic publishing, or invented remote API is involved.
- Startup support is opt-in and scoped to an exact state root. Upgrading does not
  install a service, start schedules, or activate unqualified provider adapters.

## Dependency order and ownership

1. Freeze core UI commands, policy generation, program specification, and memory
   binding contracts. Root owns CLI/TUI/core UI and broker descriptors.
2. In parallel: runtime owner implements policy, admission, reconciliation and
   schedule integration; program/memory owner implements bounded standalone VM and
   Wordcell adapters; root implements operator controls and service support.
3. Join interfaces; validate meaningful race, budget, custody, replay, boundedness,
   and UI cases. Each worker owns focused tests. Root owns the full final gate.
4. Independent review, fixes, current-head PR checks, merge. Preserve existing
   qualification guards and pinned dependencies unless a release blocker requires
   a separately tested change.
5. Publish immutable annotated v0.4.0 from reviewed main, observe the entire release
   workflow, verify actual archives, checksums and provenance, update the site's
   verified publication datum, verify deployed links, and safely install admitted
   native bytes while preserving existing state and unrelated services.

## Acceptance

The release must demonstrate automatic bounded follow-up without duplicate
dispatch, policy pause without cancelling settled custody, visible exhausted and
attention states, stale-revision rejection, explicit memory boundaries, and a
deterministic scheduled program recorded in work history. Native workspace tests,
strict clippy, formatting, compatibility checks, site checks, isolated CLI/TUI
smoke, current-head CI/security, and release admission all remain required.

Publication is independent of live provider activation. Synthetic tests do not
qualify accounts; unqualified runtime/configuration/tool inventories stay disabled.
