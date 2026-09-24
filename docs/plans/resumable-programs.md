# Resumable managed ALGAL programs

## Outcome and scope

Extend the persistent project-agent habitat with bounded ALGAL controllers that
can request ordinary xcb worker tasks, suspend durably, and resume from their
settled results. The conversation remains the project identity. Existing pure
planners and their pinned inputs remain compatible.

The first slice admits pure cells plus sequential top-level agent calls, at most
eight calls per occurrence. It does not expose arbitrary tools, executable
configuration, imports, transports or recursive programs. A current project grant
authorizes every child and pays its task budget. No new service, recurring job or
provider account is activated by installation.

## Contracts

- ALGAL authors its own canonical execution and suspension receipts. A small
  upstream programmatic host-executor API preserves replay, journal and digest
  rules. The xcb callback only reads an immutable completed-result snapshot or
  captures one request and returns suspension; it cannot launch work.
- After the interpreter slice joins, xcb atomically records its bounded receipt,
  pending call, deterministic child identity and ordinary managed child task.
  The suspended parent owns no active workspace, account or worker slot.
- Resume consumes the exact linked child's conclusive result and settlement
  identity. Restart, duplicated completion and repeated schedule ticks cannot
  create another child for that call. Uncertain execution stays held for existing
  evidence-based reconciliation; elapsed time is not settlement evidence.
- Child publication and dispatch recheck the exact grant generation, expiry,
  budget, project pause, provider constraint and parent cancellation. Normal
  backlog proposal admission is not weakened to accommodate a live parent.
- Child questions and approvals remain visible in the existing attention flow.
  The program cannot answer them. Cancellation stops new calls and propagates
  through ordinary child cancellation without prematurely claiming settlement.
- Call count, prompt/output/checkpoint sizes and interpreter work stay bounded.
  Live program dependencies are protected from retention. History keeps parent,
  child, call and receipt provenance. Replay never consumes another model call.

## Delivery graph and ownership

1. Architecture spikes — complete: VM replay/host seam and managed lifecycle.
2. ALGAL executor extension — isolated upstream checkout, one owner for source,
   tests, parity gates and upstream CI. Preserve the unrelated dirty checkout.
3. xcb program evaluator and durable orchestration — parallel disjoint modules
   after the shared step contract is frozen. Root owns CLI/UI, docs, dependency
   integration, examples, release versions and the aggregate final gate.
4. Join and independent review: restart and failure injection, exact receipt
   resumption, authority/cancellation, child attention and workspace availability.
5. Required native/compatibility/site checks, isolated operator acceptance,
   protected PR delivery, immutable release verification, publication and install.

## Acceptance and progress

- [x] Recover current source and remaining scope; preserve the assigned worktree.
- [x] Identify pinned VM replay and managed lifecycle boundaries.
- [ ] Deliver the upstream in-process executor API with offline replay tests.
- [x] Admit and evaluate managed agent-call programs without provider subprocesses.
- [x] Atomically persist suspension and children; resume, recover and cancel safely.
- [x] Expose linked waiting work and explicit program controls in existing UI/CLI.
- [ ] Validate all crash boundaries and operator flows; independent final review.
- [ ] Pass required gates and publish verified release artifacts and site.

Synthetic checks establish orchestration and custody behavior. They do not
qualify a live provider or demonstrate token/cost savings.

Independent source review required four repairs before validation: preserve full
settled child reports rather than display summaries, allow serialization headroom
for bounded call records, resolve stable-ID retries before new admission checks,
and reject overlapping immediate controllers. Regression coverage exercises each
boundary. The CLI/PTY acceptance also covers a genuine no-account child, daemon
restart, status inspection and settled cancellation without activating providers.
