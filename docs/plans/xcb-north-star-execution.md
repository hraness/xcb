# xcb north-star execution plan

This plan turns [`docs/vision.md`](../vision.md) into a durable, reviewable
work program. It is designed to run through xcb's project grant, backlog, and
pinned ALGAL scheduler. The scheduler is a herding mechanism for bounded work,
not permission to bypass repository review, branch protection, provider
custody, or release gates.

## Operating contract

- The local journal and protocol are authoritative. A worker may use a
  provider subscription only through xcb's existing account and capability
  boundaries.
- Every task owns one narrow lane, names its exact tree and validation, and
  leaves a receipt with the commit, toolchain, inputs, metrics, failures, and
  unverified claims. Workers do not rewrite or reset unrelated work.
- Parallel workers use disjoint worktrees or disjoint modules. One integration
  owner joins changes, resolves conflicts, and runs the aggregate gate. A
  worker that finds a shared contract mismatch stops at a proposal and records
  the dependency rather than guessing.
- A scheduler wake-up may inspect, implement one bounded slice, and review the
  resulting evidence. A converged integration owner may then hand the exact
  tree to the delivery cell for normal branch, PR, auto-merge, and release
  automation. It may not force-push, bypass `Required`, create an ad-hoc tag,
  expand the project grant, or declare live Valhalla/quality evidence.
- Failed experiments remain named seeds or regression cases. A metric moves
  only when its workload, host limits, provider observation, and receipt are
  comparable with the baseline.

## Phases and parallel lanes

| Phase | Owner lanes | Exit evidence |
| --- | --- | --- |
| P0 baseline | cleanup, compatibility, docs | TUI and remote entry points are absent; local JSON/SDK/task paths pass native and compatibility gates; no active Convex sync path starts from the supervisor |
| P1 protocol | wire schema, journal/events, command receipts | Versioned initialize/open/read/events/submit/cancel/answer/receipt/capabilities messages; canonical bytes, expected-revision checks, idempotency, bounded paging, and negative vectors are frozen |
| P2 SDK/projections | Rust SDK, TypeScript SDK, reference projections | Both SDKs consume the same digest-pinned vectors; `factory`, `status`, `tasks`, `attention`, and `metrics` projections are bounded, redacted, and usable by an external agent |
| P3 assurance | model, fuzz, replay, metrics | Shadow model, fault/restart drivers, deterministic replay, mutation register, assurance ledger, and baseline performance receipts cover the protocol and local kernel |
| P4 Valhalla | envelope adapter, peer journal, recovery | Signed envelopes, replay cursors, offline queues, causal convergence, custody, retention, and uncertain effects pass local qualification and bounded peer simulations |
| P5 migration | bridge, identity, operations, status | Convex state is inventoried and reconciled through an auditable, resumable bridge; Valhalla parity and recovery pass; Convex writes are disabled before code deletion |
| P6 hill climb | benchmark, portability, operations | Held-out workloads show useful settled work per attention/CPU/memory/token observation without safety, replay, recovery, or portability regression |

The lanes can work in parallel after P0 and the P1 wire contract are frozen.
P4 depends on P1 and P3's replay boundary; P5 depends on P4's parity and
recovery receipts. P6 runs continuously but can promote a change only after
the phase gates it touches pass.

## Initial backlog

These are deliberately small tasks suitable for xcb's bounded managed calls.
Each task should produce a reviewable change or an evidence-only receipt.

1. **Baseline audit and cleanup:** finish deleting TUI/remote callers,
   remove stale relay status/fault code, repair tests and help, and record the
   exact compatibility impact. Do not delete migration evidence yet.
2. **Protocol seam:** inventory existing JSON/SDK types and propose the first
   schema package, digest/version policy, canonical framing, and error model.
3. **Local event journal:** map current revisions and receipts to append-only
   events; implement idempotent replay and bounded event paging behind tests.
4. **SDK and projections:** make a small Rust/TypeScript client and a
   reference status/factory projection from the same snapshot/event contract.
5. **Testing harness:** add the shadow command/receipt model, named seeded
   fault/restart battery, golden and negative vectors, mutation register, and
   claim-to-evidence receipt format. Record absent techniques as gaps.
6. **Valhalla adapter spike:** compare the protocol boundary with Valhalla's
   signed journal/envelope APIs; implement no production switch until custody,
   replay, convergence, and uncertain-effect cases are executable.
7. **Migration design:** inventory Convex identities, remote command state, and
   host-status data; write a dry-run/recovery plan with no writes and explicit
   deletion preconditions.
8. **Benchmark and portability:** establish cold start, command acceptance,
   projection size, journal growth, CPU/memory, and native/SDK compatibility
   baselines on supported hosts.

The first task may be released immediately. The protocol, journal, SDK,
testing, Valhalla, migration, and benchmark tasks should remain independently
reviewable and may be released in parallel once their dependency is satisfied.

## Testing investment and hill-climbing

The assurance portfolio in `docs/vision.md` is the baseline classification.
Every phase records the exact claim it exercises and the unverified boundary.
The next highest-value additions are:

1. async concurrency checking around account custody, uncertain settlement, and
   subscriptions (loom/shuttle/turmoil/madsim or a bounded equivalent);
2. a bounded Jepsen-style Valhalla nemesis with packet loss, partitions, clock
   skew, and receipt/history checking;
3. coverage-guided fuzz targets for canonical envelopes, schema decoding, and
   bounded paging;
4. deterministic distributed simulation after the peer failure model settles;
5. opt-in staging chaos that preserves data and emits recovery receipts; and
6. ranking-guided exploration across projections and capability combinations.

Before those additions, keep the existing property/stateful/fault-injection
tests, model checking, symbolic/proof evidence, differential and golden
vectors, mutation, record/replay, assurance ledgers, checker attestations, and
regression promotion running. UI fuzzing moves to the reference projections
and agent clients after the TUI is gone.

The hill-climb vector is: settled useful work, operator attention, acceptance
latency, CPU, memory, binary size, projection bytes, token-usage observations,
replay/convergence time, journal growth, and portability. A candidate is
promoted only if safety invariants, capability boundaries, deterministic
receipts, recovery, and held-out workload results remain green. Synthetic
tests never become claims about provider quality, billing, or public Valhalla
availability.

## Scoped memory adoption

- **Status:** Not started
- **Depends on:** An admitted ALGAL source/view contract and immutable dependency pin; existing local command, capability, replay, and assurance boundaries. This subtrack does not depend on P4 Valhalla or P5 migration.
- **Objective:** Resume a worker with progressive detail over only its assigned context and permitted workspace history.
- **Scope:** `crates/xcb-runtime/src/managed_habitat.rs`, `managed_program_state.rs`, `context_recipe.rs` and their tests; `docs/context-recipes.md`. The integration owner owns dependency and protocol/tool declaration changes.
- **Out of scope:** A second memory database or supervisor, provider-state copying, global identity, changed retention, larger grants, removed terminal interfaces, and live campaigns without a separately frozen budget.
- **Approach:** Read the scoped-memory section of `docs/vision.md` and existing `ProgramContext` capture/query paths. Use OptMem's recent-detail/older-range pattern over ALGAL source references, not its installer or fixed-width log. Start with scripted nodes. Capture original task states, request identity, source head, grant, and derivative generation before building a view. Keep exact search and expansion independent of summary quality. Missing nodes return an explicit raw/incomplete view; reads never generate summaries.
- **Acceptance criteria:** A fresh provider receives the original task contract. Cross-workspace, cross-child, direct-session, stale-grant, and cached broader-scope reads refuse. A changed summary generation cannot shift earlier pages. Existing request/output limits include view overhead, and protected intent never silently disappears. Expired managed history is unavailable rather than presented as permanent memory. Failed and uncertain source tasks retain their states. Summary maintenance uses ordinary managed budgets and cannot release custody or retry an uncertain effect. The opt-out path preserves existing receipts and exact-context behavior.
- **Validation:** `cargo test --locked -p xcb-runtime managed_program_state_tests`; `cargo test --locked -p xcb-runtime managed_habitat_tests`; `cargo test --locked -p xcb-runtime context_recipe`; native workspace tests, clippy, fmt, and `bun run check` through the normal host controls.

An opt-in implementation can ship after those mechanism checks. Default use needs
a separately preregistered later-task comparison with correction and old-failure
cases, equal total resources including summary maintenance, fresh confirmation,
and a tested rollback. This plan records no live memory-quality result and does
not create a new schedule or authorize a provider call.

## xcb scheduler setup

The project grant is named `xcb`, lasts 30 days, and permits up to 100 bounded
follow-up tasks. A pinned controller runs hourly with five managed calls:
inspection, implementation, verification, reporting, and delivery.

```sh
xcb projects --json
xcb schedules program /Users/bg/Documents/xcb \
  examples/xcb-north-star-controller.algal.json \
  --workspace /Users/bg/Documents/xcb --managed-calls 5 --every 3600 \
  --title "xcb north-star herder"
```

The controller must inspect the current tree, grant, backlog, receipts, and
validation state before selecting work. It should keep at most one integration
owner active, release independent lanes when their dependencies are met, and
leave held work untouched when the tree or evidence is not ready. A schedule
run is not permission to bypass delivery gates. When a candidate is converged,
the delivery cell commits only task-owned changes, pushes its branch through
the configured workload identity, opens a pull request, and enables the
repository's required-check auto-merge. A merged `main` version bump is left
to the repository's annotated-tag and release workflow; the controller never
creates release tags directly. If network, credentials, checks, or merge state
are unavailable, it records the exact blocker and leaves the candidate
replayable for the next wake-up.

## Continuous delivery

Every herder cycle evaluates delivery after verification. The delivery cell
must report the exact branch and commit, push result, pull-request number and
auto-merge state, merge SHA when available, and release workflow/tag evidence.
It may publish only through the repository's normal gates and workload
identity. A failed or uncertain push is held for evidence-based reconciliation;
the controller never retries an ambiguous publication or creates a second tag.

## Definition of done

The north star is delivered when an external agent can discover xcb, operate a
real factory through the local protocol and SDK, build a useful projection,
resume after interruption, verify receipts, and—after the migration gates—do
the same across Valhalla peers. The resulting evidence must show what was
tested, what remains partial, and which metrics improved on a comparable
workload.
