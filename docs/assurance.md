# P3 assurance and comparable baselines

This page records the local assurance boundary for the protocol and journal
slice. The executable shadow model is [`src/assurance.ts`](../src/assurance.ts)
and is exported from the TypeScript package. The native local counterpart is
`crates/xcb-runtime/src/event_journal_v1.rs`, which exercises append-only typed
events and replay verification. These are pure/local fixtures: they do not
launch a provider, open a network transport, schedule work, or qualify a live
account.

## Command/receipt shadow model

A `ShadowCommand` contains a stable command ID, target, command name, bounded
JSON arguments, an expected numeric revision, and an `idem_...` key. Applying a
command records a deterministic receipt with the prior and new revision,
effect digest, manifest digest, and one of `settled`, `accepted`, `replayed`,
`rejected`, or `uncertain`. The idempotency table is retained for uncertain
outcomes. A same-body retry returns the original receipt as `replayed`; a
changed body is an `idempotency_conflict`; a stale CAS head is a
`revision_conflict`. No retry is inferred to be safe after an uncertain effect.

Receipt material contains no wall-clock time, PID, host path, provider output,
credential, or scheduler state. `replayShadowCommands` therefore produces a
stable receipt and state digest for the same command history and named routing
and projection manifest. `verifyDeterministicReplay` compares two independent
runs. `counterfactualReceiptCheck` runs the same history under a second named
manifest and reports the first differing receipt; a divergence is evidence of
a changed counterfactual, not evidence that either candidate is better.

The native event journal retains the same evidence boundary: append-only typed
events, stable IDs, causal parents, idempotency keys, bounded pages, and replay
verification. It does not claim distributed ordering or an async schedule
proof.

## Named seeded battery

`SEEDED_FAULT_BATTERY` is the fixed six-case local battery. Seeds are part of
the contract and a failing case must be promoted with its minimized history;
never discard a failure or silently retry it with a new seed.

| Case | Seed | Invariant exercised |
| --- | ---: | --- |
| `crash-before-append` | `0x5101` | no receipt is manufactured before durable append; retry may be accepted once |
| `crash-after-append-before-receipt` | `0x5102` | retry returns one receipt replay and does not apply a second effect |
| `restart-before-settle` | `0x5103` | uncertain effect remains held; restart never auto-replays it |
| `storage-write-failure` | `0x5104` | failed write does not become an accepted receipt |
| `storage-read-failure` | `0x5105` | verification is blocked explicitly rather than inventing state |
| `storage-corruption` | `0x5106` | digest corruption is rejected by replay verification |

The vectors in `protocol/assurance-v1-vectors.json` include golden command
histories and negative inputs. `test/assurance.test.ts` consumes every vector,
checks byte-independent receipt semantics, and runs the battery twice to prove
seed reproducibility. These are single-process, deterministic checks only.

## Mutation register and claim ledger

`docs/evidence/p3-mutation-register.json` names the protocol and receipt
mutations that must be killed by one or more tests. The register is an
admission map, not a claim that a mutation engine has run. A future source
mutator may add a receipt with the exact tool version and tree digest.

`docs/evidence/p3-claim-evidence.json` is the claim-to-evidence ledger. Every
row states the exact claim, evidence file/test, scope, and explicit
`unverified` boundary. In particular, local replay is not live provider
qualification, and neither is a proof of async linearizability, partition
convergence, or distributed ordering.

## Comparable baseline contract

`docs/evidence/p3-baselines.json` fixes the workload, schema, units, sampling,
and comparison rules. `scripts/assurance-baseline.ts` emits a receipt for that
workload. The required vector is acceptance latency, projection bytes, CPU,
resident memory, journal growth, and replay time. Report median, p95, and the
sample count for each run; retain cold/warm state, host limits, toolchain,
source revision, and workload digest. Compare only receipts with the same
workload and measurement method. Missing live or native observations remain
`null`, never zero.

The current P3 evidence establishes the measurement contract and deterministic
fixture baseline generator. It does not fabricate host-specific numbers. The
P6 benchmark lane may add measured native and cross-host receipts under this
schema. Synthetic numbers must not be presented as provider latency, token
savings, billing, or public transport performance.

## Deliberate gaps

The portfolio remains explicit about what is not tested here:

- no loom/shuttle/turmoil/madsim-style exhaustive async schedule check;
- no Jepsen nemesis, partition/loss/clock-skew history checker, or distributed
  linearizability claim;
- no coverage-guided fuzzer, deterministic distributed simulation, staging
  chaos, or ranking-guided portfolio exploration;
- no live provider, Valhalla peer, Convex migration, quality, cost, or billing
  qualification.

Those gaps stay visible in [`docs/vision.md`](vision.md) and must be removed
only when their own bounded tests and receipts exist.
