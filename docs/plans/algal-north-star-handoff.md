# ALGAL design input for the xcb north-star plan

This note records the ALGAL vision as design input for
[`docs/plans/xcb-north-star-execution.md`](xcb-north-star-execution.md). It is
not a production change, provider attestation, or permission to copy ALGAL
claims. The source is [`ALGAL docs/vision.md` at commit `cce01b7e`](https://github.com/hraness/algal/blob/cce01b7e/docs/vision.md), from the local checkout
`/Users/bg/Documents/algal-worktrees/site-highlight/docs/vision.md`; the source
change is `7ed19668`. Keep that exact SHA with any follow-up receipt.

## Useful concepts to carry into xcb

| ALGAL concept | xcb interpretation | Non-negotiable boundary |
| --- | --- | --- |
| Declare responsibilities and maintained outputs | Make workspace, task, projection, journal, and receipt ownership explicit. Name the state or output a task maintains and the evidence that closes it. | Declarations are validated metadata, not authority. The local xcb protocol, capability grant, revision, and custody rules remain authoritative. |
| Compile a composition into typed, bounded topology | Compile a request into digest-pinned state/event/command/receipt types, bounded pages and payloads, explicit ownership edges, and capability-scoped dependencies before execution. | Compilation cannot acquire credentials, execute effects, widen a grant, or create an unbounded/dynamic topology. |
| Run with continuity, checkpoints, ownership, and receipts | Use xcb's durable task ownership tree, checkpoint identity, provider/session identity, local journal, settlement state, and effect receipts to resume useful work. | A checkpoint must name the last settled effect and whether replay is safe, forbidden, or requires reconciliation. An uncertain effect is never silently replayed or marked successful. |
| Reconcile only changed evidence | Reconcile by stable event/effect identity, digest, and expected revision; reread only the evidence whose identity or revision changed. | A timestamp, missing response, or elapsed lease is not settlement evidence. Reconciliation must preserve idempotency, account custody, and the uncertain-effect hold. |
| Retain improvements only with evaluation, cost, and authority | Apply the fixed/held-out challenge sets, comparable workload metrics, evidence ledger, cost/host observations, rollback path, and explicit promotion authority already required by the hill-climbing plan. | Synthetic tests do not prove provider quality, billing, or public transport availability. No candidate is retained or promoted without the declared evaluation and normal delivery gates. |
| OpenProse `Requires` / `Maintains` semantics | Treat `Requires` as typed preconditions (capabilities, revisions, inputs) and `Maintains` as typed invariants/postconditions (state, projections, receipts). | Use the semantics as a review vocabulary and test obligations, not as prose-only runtime wiring or a new xcb language/runtime. |
| OpenProse compile/run separation | Keep compilation/validation separate from local execution. A compiled topology can be inspected and receipt-bound before a run starts. | Run remains under xcb's local authority. A remote transport is only an adapter over the same messages and state model. |
| Pi Durable ownership, checkpoints, and storage interfaces | Continue the ownership/checkpoint model and require local-journal and Valhalla backends to pass the same append, replay, paging, duplicate, conflict, crash/restart, retention, and uncertain-settlement conformance cases. | The interface is provider-neutral and does not import a Pi task model. It cannot weaken xcb revision/idempotency, capability, custody, or receipt contracts. |

## Rejected or explicitly out of scope

- **Hosted execution as authority:** xcb stays local-first. A hosted control
  plane is not required, and Valhalla cannot become permission to publish
  prompts, credentials, or private workspaces.
- **Prose-only wiring:** documents may describe intent, but executable wiring
  must be typed, bounded, digestable, and checked by the xcb protocol.
- **Terminal UI choices:** xcb does not adopt a permanent terminal product or
  make a TUI a prerequisite. External UIs consume the same projections.
- **Provider-specific product requirements:** no ALGAL, OpenProse, Pi, or
  provider conversation model becomes an xcb compatibility requirement. The
  protocol remains transport- and provider-neutral.
- **Unbounded or self-authorizing composition:** compilation and retention do
  not expand capabilities, create hidden workers, bypass expected revisions or
  idempotency keys, or turn an uncertain external effect into a retryable
  success.
- **Unqualified improvement claims:** ALGAL language, synthetic fixtures, or a
  routing preference cannot be copied as claims about intelligence quality,
  cost, billing, hosted availability, or provider behavior.

## Plan consequence

The concepts above refine, but do not replace, the existing xcb phases: freeze
typed topology and ownership in P1; expose maintained outputs through the Rust
and TypeScript projections in P2; bind checkpoints, reconciliation, and
retention decisions to the P3 claim ledger and comparable baselines; apply the
same storage and receipt conformance at the Valhalla and migration gates; and
make P6 promotion evidence include authority and cost as well as performance.
Any implementation or production behavior change requires a separately bounded
proposal, focused tests, and the normal xcb delivery gates. This handoff alone
changes no production or runtime code.
