# Excalibur (xcb): a portable agent factory

## North star

Excalibur should be the small, local-first execution and state kernel for any
intelligence-using system that already has access to token subscriptions. An
application should be able to embed xcb, give it a bounded workspace and
capability grant, and receive durable, inspectable work without adopting a
terminal UI, a provider-specific client, or a hosted control plane.

The interface is a protocol and SDK. A UI is an optional projection that an
agent, a web page, a desktop app, or a one-off `/status` view can build from
the same typed state and event stream. xcb owns execution, routing, custody,
hooks, evidence, and recovery; callers own presentation and product-specific
interaction.

This is a direction and a set of falsifiable claims. The agent-first change now
removes the Ratatui product surface and the fleet/remote CLI entry points. The
local execution kernel remains; the Convex code is retained only as a migration
artifact until the Valhalla replacement is qualified and the remaining relay
implementation can be deleted.

## Why this follows from the current system

xcb already contains most of the kernel needed for this direction:

- [`xcb --json route`](route.md) is a bounded provider-neutral request and
  result contract. The [TypeScript SDK](sdk.md) embeds account leases and
  qualified task adapters for applications that want to choose the account and
  model themselves.
- Managed projects, tasks, inbox events, schedules, work history, revisions,
  and receipts provide durable state for a software factory. The [managed
  harness](managed-harness.md), [project agents](project-agents.md), and
  [inbox measurement](inbox-measurement.md) documents describe the authority,
  replay, and measurement rules already in force.
- Hooks are stored with executable digests, are disabled until explicitly
  enabled, receive bounded JSON, run with a cleared environment, and fail
  closed when their executable changes ([`hooks.rs`](../crates/xcb-runtime/src/hooks.rs)).
- The Codex adapter already drives a qualified app-server protocol with
  `initialize`, `thread/start`, `turn/start`, notifications, schema and
  executable digests, and process-custody checks
  ([`codex-managed-session.ts`](../src/codex-managed-session.ts)). It is a
  useful protocol shape to learn from, not a requirement that xcb become a
  Codex client.
- The native runtime is portable and bounded. Workspace tools, command
  execution, provider launchers, account custody, and OS-confinement checks
  remain local responsibilities.

The missing boundary is a single, transport-neutral state protocol. The old
Ratatui and fleet/remote launch paths have been removed; `xcb run --json`, the
local task projections, hooks, and the SDK are the supported interaction
surfaces. The remaining Convex implementation combines two different products:
authenticated remote operations ([`remote-operations.md`](remote-operations.md))
and an anonymous host-status heartbeat ([`host-status.md`](host-status.md)).
Neither is the general event and projection protocol that external agents need.

## Product contract

### Agent first

An external agent must be able to do everything required for ordinary operation
without drawing a terminal:

1. discover protocol and capability versions;
2. open or resume a workspace and conversation;
3. read a bounded snapshot and page historical events;
4. submit, steer, cancel, answer, or approve a command with an expected
   revision;
5. subscribe to typed state changes and attention items;
6. obtain a settled receipt, usage observation, and evidence links.

The protocol should be modeled after the useful parts of the Codex app-server
SDK: explicit initialization, request IDs, typed responses, notifications,
versioned schemas, and capability negotiation. It must add xcb's own rules for
workspace authority, revision checks, bounded pages, idempotency keys, account
custody, and uncertain effects.

The first transport is local stdio or a local Unix socket. A process embedding
xcb must not need a network account. Remote transports are adapters over the
same messages, never alternate state models.

### Operating layer for existing subscriptions

xcb should let an agent operate the user's Claude and Codex subscriptions
through one durable interface: discover capabilities, choose an available
account and model, inspect context and usage, submit work, steer or stop it,
and read verified progress after a restart. It should add coordination and
recovery around provider sessions without substituting API billing or
weakening provider policy.

Both adapters need explicit method accounting and tests for startup,
streaming, tools, limits, denials, cancellation, and recovery. Every method
in a checked provider protocol must be identified as implemented, handled
by xcb, or unavailable. Account sign-in, model discovery, source tests,
and live command acceptance are different observations. A broad catalog
must not imply that disabled provider controls are callable.

Session resume/fork, live steering, context management, MCP changes, and
background work should use xcb's durable task and command contracts. They
must retain account identity, workspace grants, context lineage, expected
revision, process ownership, and effect records. Read-only diagnostics come
first; mutations require their own tests and relevant live acceptance.
Raw provider calls are not the agent-facing interface.

### State, events, commands, and projections

The stable contract has four layers:

- **State** is the latest bounded projection: workspaces, conversations,
  tasks, attention, inbox, schedules, account availability, capabilities, and
  the current factory health summary.
- **Events** are append-only facts with stable IDs, an origin, a sequence,
  causal parents, a schema version, and a digest. Replaying the same event is
  idempotent; changing its body under the same ID is a conflict.
- **Commands** request a state transition. Each command names its target,
  authority grant, expected revision, idempotency key, and deadline. A command
  receipt distinguishes accepted, prepared, settled, rejected, and uncertain.
- **Projections** are named, bounded views over state and events. `factory`,
  `status`, `tasks`, `attention`, and `metrics` are useful initial projections;
  consumers may render them however they choose.

The initial method and notification vocabulary should be small and explicit:

```text
initialize
workspace/open       workspace/read       workspace/events
command/submit       command/cancel       command/answer
receipt/read         capabilities/read    metrics/read
state/changed        attention/changed    command/updated
```

The exact wire schema belongs in a generated, digest-pinned protocol package.
The native Rust SDK and the TypeScript SDK should expose the same concepts,
with async event iteration, bounded paging, typed command builders, and
receipt verification. No SDK import should start a service, discover a
credential, or silently publish data.

### Custody and trust

Local execution remains the authority for credentials, provider processes,
workspace access, hooks, and account leases. A remote caller receives only the
capabilities explicitly granted to its workspace and run. It cannot infer
process exit from a response, release an uncertain account, expand a project
grant, or acknowledge input that was clipped from a prompt.

Hooks and external projections consume sanitized, bounded events. They do not
receive provider secrets or arbitrary host paths. Every effect that can be
retried has a stable identity and a settlement receipt.

### Native provider execution

Claude Code and Codex should run with their own native file, shell, and
network tools in the granted workspace. xcb should coordinate accounts,
routes, durable tasks, observations, and recovery around those sessions rather
than replace ordinary development with offline command replay. This applies to
both supported providers; native execution is not a Codex-only desktop capability.
After each backend passes its activation checks, native execution should be the
normal choice for factory coding tasks. Brokered isolation remains an explicit
caller choice, not an invisible substitute for missing native capabilities.
Devin execution remains retired. Its historical account and session tags stay
readable for recovery, but cannot acquire new native execution grants.

Native execution is an explicit, persistent task requirement. Resume,
continuation, quota failover, and cross-provider handoff preserve that
requirement and the workspace capability grant. They cannot satisfy it with
broker-only tools or silently fall back to the offline VM. The existing
brokered execution mode and offline command runner remain separate options for
callers that need their narrower isolation contract.

Each native backend uses a versioned provider transport: Codex app-server or
Claude's structured stream. It preserves supported provider approval controls
and organization policy. xcb must verify Claude Auto and Codex automatic
approval review independently; similar mode names do not establish equivalent
permissions. Additional approval requests and denials stop automatic work
rather than grant permission through a different provider.

The host validates the effective native tool inventory, filesystem and network
limits, isolated account/configuration state, and exact executable before
activation. A broker-only provider receipt does not establish these claims.
Workspace shell tools and authorized Git/network operations require their own
live acceptance cases. Native mutations carry durable effect identities;
timeouts, missing replies, and process exit alone do not make those mutations
safe to replay. Unknown or partially settled effects retain account custody.

The macOS implementation exposes `workspace_native_exec` through each
provider's native tool protocol. xcb runs the command in the granted worktree
under an OS policy with DNS and TCP 443 access, a private command home, and
process records tied to the owning task and account. Offline replay and the
legacy Codex host tool cannot satisfy a native task. Provider-built-in host
shell and web tools are not enabled by this implementation; enabling them
requires separate checks of their file access, credentials, and approval
behavior. The target above includes those tools, not only the host bridge.

Before a workspace can use native commands, `xcb native qualify` must pass the
OS checks and `xcb native verify --provider <provider>` must record a successful
provider command, HTTPS request, and Git operation for the running executable.
The optional `--github` check uses read-only authenticated requests before
host GitHub credentials can be granted. `xcb native grant` then records the
workspace, providers, and additional toolchain or Git directories selected by
the host. Qualification sessions use disposable workspaces and one
session-bound command; they do not enable native access to real projects.
Linux and Windows native commands remain unavailable rather than run without
OS confinement. These checks do not claim live remote writes, provider-built-in
approval classification, or all planned acceptance cases.

On the development Mac, Codex passed native shell, DNS/HTTPS, local Git,
and read-only authenticated GitHub checks through this command bridge,
including with the login service's system-only environment. The installed
supervisor then completed the same checks in a durable managed task:
`t_1eb83a062043af67d63e59c04318fe6d1eeabc1a82fb8219c719940f6914f0b6`.
It made one native command call, returned all four expected markers with exit
code zero, confirmed process exit and settled effects, and produced one local
Git commit. Its task record verified. The tested executable's SHA-256 is
`ff2b38dab24fe2b9923327ded6158189151bf78d79598134a9e19dd57cdeacc7`.
Host credential helpers resolve trusted absolute toolchain paths independently
of the restricted PATH supplied by the login service. No remote write was tested.

Claude's effective tool boundary passed startup checks, but an earlier
account's signed-in session reported that the organization had disabled
Claude Code subscription access. A later check confirmed the supported Claude
build and refreshed an existing account's model catalog through startup and
usage-metadata requests,
without sending a coding prompt. All 46 targeted offline Claude tests passed.
The account's quota and reset time remained unmeasured. These checks do not
establish usable subscription access or clear the earlier organization refusal.
A newly signed-in account then passed the Auto-mode startup checks on an
account-pinned Sonnet session, but Claude rejected its prompt at the weekly
usage limit, with a reported reset of 2026-10-06 at 03:00 UTC. Its process
exited with no tool effects, and xcb recorded the account's quota block.
Haiku was unsuitable for this check because it does not support Auto mode.
The [provider method stages](provider-permissions.md#provider-method-coverage)
add account-pinned native verification, startup/account inspection for both
providers, Auto-compatible Claude catalogs, and one-shot cooperative
cancellation. The task build accounts for all 262 methods in the checked
Codex wire schema and all 29 pinned Claude SDK Query methods. Claude can
request summary-only context and redacted MCP status for its metadata
connection; this is not inspection of an active coding task. Codex metadata
inspection creates no thread, submits no prompt, and consumes no reset
credit. `xcb native status` adds a bounded local account/session/run/tool
projection with exact filters, versioned JSON, JSONL records and cursors,
while excluding transcript-derived titles and prompt text. Offline tests
check method drift and refusal before execution; method accounting does not
establish exhaustive live acceptance. Session,
MCP, settings, and rewind controls retain separate scope, inventory, and
recovery gates. These additions do not qualify Claude
while subscription usage is exhausted or replace the validated live daemon.
Claude native execution remains unavailable until authorized subscription
usage and live acceptance succeed. Devin remains retired. Existing project
grants and uncertain runs do not acquire wider access from these test results.
[`native_backend.rs`](../crates/xcb-runtime/src/native_backend.rs) keeps the
planned acceptance list separate from recorded availability.

### Durable task ownership

Long-running work is a bounded ownership tree, not a flat list of provider
turns. Each task records its parent (when any), workspace owner, foreground or
background policy, cancellation propagation, required or optional join
children, and the checkpoint and effect receipts that establish its progress.
A parent cannot settle while a required child is unsettled. Cancellation,
detachment, and child failure are explicit state transitions; none may widen a
capability grant or turn an uncertain effect into a successful result.

A checkpoint identifies the logical step, input and capability digests,
provider/session identity, last settled effect, and next resumable step. It
must say whether replay is safe, forbidden, or requires reconciliation. This
is the boundary between resuming useful work and accidentally repeating an
external mutation after a provider or host failure.

### State-driven progress and recovery

Task completion should follow owner-declared predicates over current state,
required child results, and effect evidence. Safety invariants must hold during
all work; satisfying them alone does not finish a task. A provider's final
message or an ALGAL planner's completed slice is not independent evidence
that the product's acceptance criteria have passed.

Each proposed step should name the state it observed, its permissions, and the
version of the checks it must satisfy. The host validates those references
before starting new work, records one limited transition, and observes again.
A stale proposal needs reconsideration. The proposer cannot alter the running
kernel, project grant, evaluator, or completion conditions through an ordinary
task transition. Reviewed source changes produce separately checked artifacts.
These requirements extend the local protocol; they do not add methods
or fields to the [frozen P1 slice](protocol-seam.md) by implication.

Recovery choices should remain durable attention items accessible through the
same protocol and SDK as other task state. Each choice binds the failed step,
current revision, and granted capability, so another agent can resume or stop
without reconstructing a terminal session. A rejected pure proposal leaves
accepted state unchanged. A possibly executed effect retains its identity and
account ownership until independent evidence establishes how it ended; a
snapshot or expired deadline cannot release the account or authorize a retry.

### Replaceable storage with one contract

The local journal and Valhalla transport are implementations of one storage
contract. A backend must pass the same conformance cases for append, replay,
bounded paging, duplicate delivery, body conflicts, crash recovery, restart,
retention, and uncertain settlement. Storage replacement is admitted only
with those receipts and with migration evidence that preserves task ownership,
event identity, and effect receipts. A new backend does not create a second
state model.

## Transport direction: Valhalla replaces the relay

Valhalla is the intended transport for shared xcb state. Its signed messages,
portable peers, journal/replay model, and convergence rules fit xcb's need to
coordinate intermittent agents without making a hosted provider authoritative.
It also gives us a valuable dogfood loop: xcb can exercise Valhalla with real
task events, custody transitions, and status projections.

The Convex schema and deployment sources remain as migration evidence, not an
active native transport. The local supervisor no longer starts the relay or
host-status heartbeat, and the hosted remote commands are removed. No hosted
data is deleted by this baseline. Valhalla receives no production traffic until
its identity, replay, custody, and recovery checks pass.

Migration is a gated sequence:

1. keep the local journal authoritative and define the transport-neutral event,
   command, and receipt schemas;
2. implement a Valhalla adapter with signed envelopes, replay cursors,
   idempotent delivery, causal convergence, bounded retention, and explicit
   offline/uncertain states;
3. qualify enrollment, authorization, remote commands, recovery, and status
   projections against the same acceptance cases as the local path;
4. migrate existing Convex identities and pending operations through a
   resumable, auditable bridge, preserving user data and reconciling uncertain
   writes before retrying;
5. disable Convex writes, observe the Valhalla path, and remove the relay only
   after the production invariants hold.

Until step 4, the code and docs must not imply that Valhalla syncing already
exists. A local-only installation must remain fully useful throughout.

## TUI removal and delivery sequence

The first implementation increment removes the Ratatui product surface and
remote fleet commands, while retaining useful terminal primitives needed for
sign-in and bounded input. This baseline is now in the tree; the remaining
steps extend the protocol and replace the compatibility relay.

1. Freeze the protocol and projection contracts, then inventory every former
   `xcb-tui`, `xcb chat` / `xcb resume`, and fleet/remote caller, test, help
   string, and release check.
2. Move the default interaction to the local protocol and SDK. Preserve
   equivalent agent operations through JSON and typed projections; the shipped
   CLI now rejects the removed interactive and remote entry points.
3. Keep `xcb run --json`, task controls, account sign-in prompts, and other
   bounded terminal input where they remain useful, while deleting the old UI
   crate and its rendering dependencies.
4. Ship a small protocol client and reference status projection so another
   agent can operate xcb and build a UI on demand.
5. Add a Valhalla transport behind the same protocol, then execute the relay
   migration above.

The TUI removal is a behavior change and needs an explicit release note,
updated compatibility docs, a migration path for saved conversations, and
operator acceptance proving that every former daily operation has an agent or
JSON equivalent. It should not be hidden inside a refactor or inferred from a
crate deletion.

The kernel may expose signed, capability-scoped extension manifests for
adapters, projections, or hooks. Extensions carry an executable or schema
digest and an explicit owner; mutable ambient extensions and provider-specific
UI behavior remain outside the kernel contract.

## ALGAL intersection and hill-climbing

xcb should accumulate tested ways of running and coordinating intelligence,
rather than merely dispatching more prompts. Every reusable procedure, route,
projection, and hook records its version, input contract, evidence digest,
measured cases, and promotion decision. A changed procedure is a new candidate
until it passes its declared challenge set and the relevant custody checks.

The long-term objective is useful, settled work per unit of operator attention
and host overhead, subject to zero tolerance for dropped commands, false
acknowledgements, unauthorized capability use, or automatic replay after an
uncertain effect. The following measurements make that objective testable:

| Area | Metrics and evidence |
| --- | --- |
| Agent ergonomics | time to initialize, time to first accepted command, commands per accepted outcome, projection bytes, and the percentage of operations completed without a UI-specific path |
| Correctness | duplicate, dropped, clipped, stale, unauthorized, and uncertain command counts; replay convergence; receipt verification failures |
| Work quality | task acceptance rate, resumable completion rate, attention resolution time, and provider usage observations keyed by stable identity |
| Factory efficiency | account idle time, quota lost to avoidable routing, provider start and cleanup latency, event batching, CPU, memory, binary size, cold start, acceptance latency, projection bytes, journal growth, and replay time |
| ALGAL learning | qualified procedure reuse, challenge pass rate, regression rate on held-out cases, evidence age, and rollback frequency |
| Valhalla transport | signature rejects, offline queue age, replay time, convergence time, peer availability, bounded journal growth, and migration reconciliation count |
| Portability | successful native and SDK qualification on macOS, Linux, Windows, and embedded/headless hosts under the same protocol tests |

Metrics are observations, not claims of improvement. Token savings require
provider-reported usage, billing claims require billing evidence, and quality
claims require the same acceptance checks on the same workload. Synthetic
protocol tests prove replay and custody behavior; they do not qualify a live
provider or prove lower cost. The P3 baseline contract fixes a workload,
seed, warmup, repetition count, units, aggregation, and comparison identity in
[`docs/evidence/p3-baselines.json`](evidence/p3-baselines.json); its generator
is [`scripts/assurance-baseline.ts`](../scripts/assurance-baseline.ts). A
contract-only receipt has null host-specific observations rather than invented
numbers.

## Assurance portfolio and hill-climbing loop

The north star includes a testing system that can tell a useful improvement from
a fast but unsafe one. Each protocol, transport, routing, hook, and projection
version carries a claim record: the invariant or user outcome, the test layer,
the exact inputs and tool digests, the bounded evidence, and the cases that are
explicitly unverified. A green unit test never silently becomes a live-provider,
cloud, or quality claim.

The current cross-project portfolio classifies the techniques as follows. The
scope column is deliberately narrow: it says what the evidence exercises, not
what a nearby test might suggest.

| Technique | Status and location | Claim actually exercised | Gap or next action |
| --- | --- | --- | --- |
| Pure property-based testing | ✅ Rust `proptest` in xcb and ALGAL; fast-check in the TS portfolio; Hypothesis in Python | Round trips, parser/normalizer laws, bounded record invariants | Keep vectors receipt-bound and add protocol schema properties to both SDKs |
| Stateful/model-based PBT with shadow oracle | ✅ ALGAL interleaved draw model; fast-check production-reducer schedules; xcb `src/assurance.ts` shadow command/receipt model | The model and reducer agree for generated local histories; xcb CAS/idempotency receipts are stable | Preserve shrunk histories and add protocol schema properties to both SDKs |
| Single-process fault-injection PBT | ✅ Generative crash, uncertain-commit, and storage-fault drivers; gobstopper custody model; xcb named six-case battery | Recovery and custody invariants under injected local failures | Add Valhalla journal and provider-process fault adapters |
| UI fuzzing/autonomous UX exploration | ✅ Bombadil CI campaigns, diagnostic only; failures promoted to regressions | Reachability and diagnostic UX behavior for explored UI paths | The removed TUI no longer needs this gate; apply the same driver to reference projections and agent clients |
| Seeded bespoke fuzz and bounded stress | ✅ aicharts-fuzz named seeds; gobstopper deterministic 159-test suite and nightly multiplier; xcb `0x5101`–`0x5106` battery | Reproducible stress and known crash/restart/corruption cases | Add named xcb protocol seeds and a bounded long-run local journal battery |
| Coverage-guided fuzzing | ❌ No cargo-fuzz/libFuzzer/AFL targets | No coverage-guided claim is made | Rank a small parser/envelope target after protocol schema freeze |
| TLA+ model checking | ✅ Pinned TLC inventories and mutants in Valhalla, ALGAL-cloud, gobstopper | Model invariants, counterexamples, and witnesses for the specified cases | Add xcb custody/receipt model or explicitly bind xcb to an existing model |
| Quint/Apalache plus production trace replay | ✅ Ghostget seeded simulation, bounded checking, mutants, and ITF replay | Model traces replay into production code for the declared adapter | Reuse the trace format for xcb/Valhalla convergence |
| Symbolic model checking | ✅ Kani spent-nonce and authorization harnesses | Finite symbolic authorization and nonce properties | Add only where xcb has a similarly finite security boundary |
| Exhaustive interleaving explorer | ✅ ALGAL `ordering` enumerates delivery/dispatch orderings | All enumerated terminal states satisfy its invariant | Enumerate xcb event/receipt orderings before claiming distributed linearizability |
| Async concurrency model checking | ❌ No loom, shuttle, turmoil, or madsim | No claim about all real async schedules | Highest-value bounded targets are command settlement, account custody, and Valhalla replay |
| Jepsen-style nemesis and history checking | ❌ No partition/loss/clock-skew nemesis or Elle/Knossos checker | No real multi-node consistency claim | Add after Valhalla peer protocol stabilizes; start with bounded partitions and receipts |
| Deductive verification | ✅ Verus inductive proofs and required proof mutants | Stated model invariants and mutant violations | Keep proof assumptions linked to executable conformance vectors |
| Theorem proving | ✅ Lean 4 quorum, canonical JSON, ledger projects with axiom audits | The theorem under the audited axioms and corpus | Add a canonical-envelope proof only if it becomes a wire compatibility requirement |
| Refinement proof model→code | ❌ Correspondence is tested, not extracted/proved | No formal refinement claim | Treat conformance tests and trace replay as the current boundary |
| Cross-implementation differential | ✅ Dual-runtime parity and byte-identical receipts | Implementations agree on the declared receipt and codec vectors | Make Rust/TS xcb protocol parity a release gate |
| Independent oracles | ✅ Dev Rust JCS/URL oracles and sampled model-vs-production checks | Implementation agrees with an independently written oracle | Keep oracle code isolated and forbid shared bug-shaped helpers |
| Golden vectors and fixtures | ✅ Frozen cross-implementation and standard-library vectors; xcb protocol and assurance vectors | Exact bytes, schemas, error categories, and named local receipt outcomes stay compatible | Publish versioned xcb/Valhalla vectors with more transport-negative cases |
| Explicit metamorphic relations | ◐ Named xcb idempotent replay, page slicing, redaction, and projection-order relations are partially catalogued | Some transformations preserve outcomes; counterfactual manifest changes are reported as divergence | Complete relation catalog when projections and transport are frozen |
| Spec mutants | ✅ Mutant configurations require expected counterexamples | The test suite detects specified invariant violations | Add mutants for xcb expected-revision and capability narrowing |
| Proof-level mutants | ✅ Verus mutant proofs | Proof obligations fail when the model is weakened | Keep mutant receipts in the assurance ledger |
| Source mutants | ✅ Nightly register maps each defect to the one test that must fail; xcb `docs/evidence/p3-mutation-register.json` registers receipt mutants | Regression tests detect mapped source defects | Run the register on a pinned tree and retain receipts |
| Record/replay determinism | ✅ Platform property: no wall-clock in receipts; xcb `verifyDeterministicReplay` and native journal verification | Replayed local effects verify bit-for-bit where promised | Extend the receipt boundary to transport envelopes and SDK event iterators |
| Counterfactual replay | ✅ Revised manifests compare recorded effects; xcb `counterfactualReceiptCheck` names the first divergence | The system reports identical or divergent outcomes under a changed manifest | Add routing and projection counterfactuals to release evidence |
| Deterministic contention probes | ◐ Serial CAS-head race records | Contention cases are reproducible probes, explicitly not linearizability | Keep the narrow wording; pair with async/convergence testing |
| True deterministic simulation | ❌ No Antithesis/FoundationDB-style DST SDK | No system-level deterministic distributed execution claim | Defer until Valhalla's failure model and adapter boundaries are stable |
| Assurance ledgers | ✅ Claims→evidence registries with explicit not-verified scopes; xcb `docs/evidence/p3-claim-evidence.json` | Reviewers can inspect why a claim is admitted | Make the xcb north-star milestones consume one ledger format |
| Checker attestation | ✅ Pinned checker hashes, receipt-bound inputs, RunnerProbe audits | The named checker ran on the named tree and inputs | Bind SDK and Valhalla qualification receipts to the same attestation |
| Regression promotion | ✅ Shrunk failures and seeds become named example tests; xcb vectors and six-case battery are ordinary tests | A found failure stays reproducible in ordinary CI | Require promotion before a fuzzer seed is discarded |
| Deployed chaos engineering | ❌ No production nemesis | No claim about live partition or degraded-service behavior | Add opt-in, data-preserving Valhalla staging chaos before production activation |
| Ranking/coverage-guided exploration at scale | ❌ Bombadil is the only feedback-driven fuzzer | No portfolio-wide exploration optimization claim | Rank protocol paths by failure history, state novelty, and untested capability edges |

For this codebase's risk profile, the absent work is ordered by marginal
bug-finding value: (1) async concurrency model checking around account custody,
uncertain settlement, and event subscriptions; (2) a bounded Jepsen-style
Valhalla nemesis with receipt/history checking; (3) deterministic protocol
simulation/DST once the peer adapter exists; (4) coverage-guided fuzzing for
canonical envelopes, schema decoding, and bounded paging; (5) staging chaos
against the Valhalla transport; and (6) ranking-guided exploration across
projections and capability combinations. Refinement proofs remain a later
investment because the protocol's first risk is an incorrect boundary and
recovery behavior, where executable models and independent histories find bugs
sooner.

Every hill-climb must run a fixed challenge set and a held-out set, compare the
same workload before and after, and publish a receipt containing the candidate
version, seed, toolchain, host limits, metrics, and failures. Optimize a vector
rather than one score: settled useful work, operator attention, latency, CPU,
memory, token usage observations, replay/convergence time, and binary size are
reported together. A candidate is promoted only when it improves the declared
objective without regressing safety invariants, portability, recovery, or
reproducibility. Failed candidates remain evidence and become seeds or
regressions; they are never silently retried until they pass.

## Milestones and gates

- **M0 — contract:** publish the protocol schema, projection names, command
  receipts, capability model, compatibility policy, and measurement definitions.
- **M1 — agent-first local kernel:** remove the Ratatui surface, preserve
  agent/JSON parity, and pass native, compatibility, recovery, and operator
  acceptance checks.
- **M1N — native workers:** implement Claude Code and Codex native
  tool backends under the same durable task contract. For each exact runtime,
  pass the declared native inventory, shell/toolchain, DNS/HTTPS, workspace
  confinement, private account/configuration, authorized Git effects, approval
  readback/denial, descendant cancellation, uncertain recovery, resume, and
  cross-provider handoff cases before activation. Promote native execution to
  the normal coding path only with rollback evidence; keep existing tasks and
  uncertain runs under their original execution grants during migration.
- **M2 — SDK and projections:** release Rust and TypeScript clients, a reference
  `/status` projection, schema-digest checks, and external-agent examples.
- **M3 — measured harness:** connect ALGAL evidence and hill-climbing to route,
  procedure, hook, and projection versions without enabling unqualified
  self-modification; consume the P3 shadow model, seeded battery, replay,
  mutation register, claim ledger, and comparable baseline receipt.
- **M4 — Valhalla transport:** pass signed replay, offline, convergence,
  custody, and recovery qualification against the local protocol.
- **M5 — migration:** reconcile Convex identities and pending work, switch
  remote operations and status to Valhalla, observe production invariants, and
  remove Convex writes.
- **M6 — portability:** repeat the protocol and performance gates across
  supported hosts and embedded runtimes; publish only measured limits.

No milestone makes a hosted service mandatory for local xcb. No milestone
turns an experimental provider, hook, procedure, or transport into a default
without exact qualification and a rollback path.

## Non-goals

- xcb will not own a permanent terminal, web, or desktop UI.
- xcb will not proxy provider APIs or require callers to adopt one provider's
  conversation model.
- Valhalla will not be treated as permission to publish prompts, credentials,
  or private workspaces; callers still grant explicit capabilities.
- ALGAL evidence will not be used to claim intelligence quality from routing
  preference, subscription quota, or synthetic fixtures alone.

The north star is achieved when a new agent can discover xcb, operate a real
software factory through the protocol, build the view it needs from typed
state, resume after interruption, and verify what happened—on a laptop or
across a Valhalla-connected set of peers—without knowing which provider or UI
implemented the work.
