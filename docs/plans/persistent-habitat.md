# Persistent project agents and automatic routing

## Outcome

A project agent is an existing managed conversation bound to a workspace. Its
backlog, recurring instructions, attention requests and work history persist
across terminals, daemon restarts and provider replacement. The user guides the
project; xcb chooses an admitted model for each ready task.

Recovered context: Devin `petite-pyrite` (2026-09-23), continuing `basalt-bard`
and the intervening Claude session. Launch-polish and dependency work landed
through PRs 136–138. This feature starts at main `24a2e84` and preserves those
sessions' checkouts and local changes. Live cross-provider acceptance and the
newer Claude SDK's requalification remain separate from synthetic validation.

## Contracts

- **Identity:** reuse `ManagedConversation` for the persistent agent and
  `ManagedTask` for dispatched work. Provider sessions carry execution context;
  changing provider never changes project identity or grants new authority.
- **Routing:** integrate ALGAL's six-question fitted model-router through the
  existing typed judge. Deterministic fallback works without judge credentials.
  Substantial prompts (400 words or 8 KiB) request the highest known capability
  among admitted eligible routes. Account custody, qualification and explicit
  provider/model constraints always apply first. Quota degradation must be
  explained from observed quota evidence. Model quality remains a policy
  estimate; model context-window support is not inferred from prompt length.
- **Backlog:** distinguish deferred planning from ready execution. User chat and
  the CLI can create, edit and release backlog work. Agent-authored follow-ups
  stay in the originating project and cannot authorize themselves to expand
  scope. Version checks prevent lost updates; stable worker call identities
  prevent duplicate mutations on replay. Completed managed work is already a
  history entry and must not create a duplicate summary task.
- **Scheduling:** the xcb supervisor owns wall time. Store interval, next due
  time, enabled state and last occurrence durably. Coalesce missed intervals;
  prevent overlapping work and duplicate dispatch after restart. Pausing never
  implies cancellation of a running effect. No schedules are activated merely
  by upgrading xcb. Human attention and uncertain effects block recurrence.
- **Attention:** preserve answer, approval and action statuses, aggregate them
  across project agents, and navigate to the owning conversation/task. A reply
  is explicit input, never proof that an external approval occurred. Existing
  admission and provider approval boundaries remain authoritative.
- **Coordination:** same-workspace discovery and durable messaging remain the
  authorized default. Expose backlog and recent work through bounded typed
  tools. Messages are data; they cannot change another task's authority.
- **Memory:** recent settled summaries are a bounded local working set, ordered
  newest first and carrying task identity and status. They are historical
  reports, not fresh observations. ALGAL's dependency-aware fact/derivation
  memory is useful for validated observations. Wordcell remains the external
  long-term knowledge source. Do not silently copy private histories to it or
  treat cached claims as current evidence. Promotion should retain provenance,
  workspace scope and applicability dependencies.
- **ALGAL:** `algal.process.v1` is a generation-bounded process with mailbox
  wakes, not a wall-clock service. xcb supplies persistence and timer events;
  each admitted program invocation retains VM fuel, generation, host-effect and
  receipt limits. Do not make the VM unbounded or expose a shell through the
  scheduling or backlog tool contracts.

## Delivery graph and ownership

1. Recover transcript and inspect routing/habitat implementations — complete.
2. Freeze contracts above and implement in parallel:
   - routing_spike: routing and task classifier;
   - habitat_spike: managed store, schedule/backlog, supervisor and working set;
   - session_recovery: native UI contracts, TUI and CLI;
   - root: broker surfaces, integration, design and validation.
3. Join: compile all contracts, test intake → backlog → dispatch → attention or
   settlement → recent work, schedule replay, and quota routing.
4. Independently review the converged change; repair and rerun affected checks.
5. Run repository gates, isolated CLI/TUI smoke, and report exact delivery state.

Workers share one feature worktree with disjoint ownership. The integration
owner alone runs aggregate gates. Keep the existing shared Cargo target cache
and host scheduler; never run smoke commands against real user state.

## Validation and evidence

Required native gates: workspace tests, clippy with warnings denied, rustfmt.
Compatibility gate: `bun run check`. Run the site gate only for site changes.
Synthetic provider fixtures prove protocol and custody behavior, not account
availability or live provider acceptance. Live qualification is separately
reported and never guessed from stored model catalogs.

## Delivered foundation (2026-09-23)

The implementation and independent review are complete for automatic routing,
backlog and work-history tools, prompt interval schedules, attention aggregation,
and bounded recent working memory. The native TUI and CLI expose the same
revision-safe controls. A final review also covered quota-only unavailability,
command alias collisions and retained uncertainty.

All required local gates passed: workspace tests, strict clippy, rustfmt, the
compatibility check (1,064 tests plus package smoke), and the site check
(41 source tests plus production runtime acceptance). Final isolated CLI/PTY
acceptance passed 23 checks against the rebuilt binary. It exercised persistence,
backlog edit/release, stale revisions, priority preservation, timer pause,
attention/history views and aliases; its owned daemon exited cleanly.

The current boundaries and next integration contracts are in
[`project-agents.md`](../project-agents.md): autonomous proposal admission,
router-specific clarification, scheduled ALGAL manifests, in-place managed-task
uncertainty resolution and Wordcell synchronization remain follow-up work.
