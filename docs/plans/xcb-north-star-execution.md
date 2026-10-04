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

## Next local increment: state-driven progress and recovery

This increment refines P1–P3 before expanding remote execution or autonomous
policy changes. The [protocol seam](../protocol-seam.md) and
[assurance record](../assurance.md) describe the implemented slices; the
requirements below are follow-up work, not claims that the full loop ships.

1. **Completion contract:** distinguish safety invariants from task completion
   predicates. Bind predicates and evaluator versions to the task owner and
   accepted inputs. Keep a provider summary or
   `managed_program::ProgramSliceOutcome::Complete` as execution evidence,
   never a substitute for product acceptance, required joins, or settled effects.
2. **Observe and commit one step:** bind each proposal to its observed revision,
   input and program digests, project grant, and checks. Validate again before
   a new dispatch or publication. Preserve the existing atomic
   checkpoint/child publication in `managed_program_state.rs`; stale work must
   not dispatch, overwrite a newer head, or manufacture completion.
3. **Durable recovery choices:** expose bounded choices through task/attention
   projections and the SDK, tied to the failed command, expected revision, and
   capability grant. Re-observation, verified checkpoint continuation,
   reconciliation, and stopping have distinct meanings. No permanent TUI or
   prompt-only restart menu is required. Wire changes require versioned
   schemas and new Rust/TypeScript vectors rather than extending P1 silently.
4. **Acceptance battery:** retain named cases for stale observation, changed
   permissions, a rejected pure proposal, false completion from model text,
   unresolved required children, crash before dispatch, and crash after a
   possibly executed effect. Include a restart while awaiting a recovery
   choice and a changed-head rejection of that choice. An uncertain effect
   must remain held without a second dispatch, and the prior evidence remains
   replayable.

These cases extend P3's local ledger and both SDKs' conformance tests. Live
provider process-exit and sandbox evidence remain separate requirements for
activation; synthetic protocol evidence does not qualify them. Existing
program budgets and generation limits remain in force, so a task that has not
met its predicates stops, waits, or reports exhaustion rather than looping
without a limit.

## ALGAL consumer coordination

xcb is an ALGAL consumer through `crates/xcb-runtime/Cargo.toml`, not only a
reference for ALGAL's architecture. Its native dependency, `Cargo.lock`,
managed-program adapter, digest-pinned controllers, persisted checkpoints,
and scheduled occurrences belong in ALGAL's portfolio adoption register.

For each relevant ALGAL change, the ALGAL integration owner coordinates a
bounded xcb task with an xcb owner, supported immutable revision, changed
contract scope, conformance cases, and recovery plan. The xcb owner verifies
native replay, checkpoint/child publication, permissions, provider ownership,
and protocol/SDK compatibility before delivering the update. A core repin
alone cannot establish those outcomes.

Previously accepted occurrences and historical evidence keep their original
program and effect identities. Re-admit future controllers through the normal
schedule and project-grant path; do not rewrite existing schedules, grants, or
checkpoints to pick up a new runtime implicitly. Publication of ALGAL and
activation of xcb remain separate decisions under each repository's checks.
A compatibility hold has an owner and expiry and stays visible as unfinished
portfolio adoption work. This plan update changes no running schedule.

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
