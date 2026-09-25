# Effectful ALGAL daemons

## Outcome and scope

Extend the managed habitat with named, durable ALGAL processes
(`algal.process.v1`): bounded programs that persist across ticks, wake on
mailbox deliveries and timer events, and request ordinary xcb worker tasks as
their only provider effect. A daemon survives supervisor restarts, machine
reboots and workspace idle; it never owns a provider session, workspace,
account or worker slot while suspended. The conversation remains the project
identity; a daemon belongs to exactly one conversation.

This slice admits the same program profile as managed controllers — pure
`input`, `const`, `fn` and `expr` cells plus sequential top-level `agent`
cells (at most eight per process, text output, no route/retry/shadow) — and
two host tool drivers, `mailbox.receive.v1` and `mailbox.send.v1`, over
capability ports. There are no imports, transports, arbitrary tools,
subprocess backends, slot/spawn cells, recursive programs or wall-clock
reads. `algal.process.v1` generations are the only durability model; no
persistent VM store, no self-modifying orchestration.

## Contracts

- **Store.** `algal::process::ProcessService` and `MailboxService` share one
  filesystem store rooted under the private state directory
  (`<managed>/daemons/`). Process names have lifetime uniqueness; the record
  chain (ready → intent → outcome, ≤64 generations) is the only daemon state.
  The managed store gains `daemon_meta` (one row per named process) and
  `daemon_calls` (one row per request digest), which binds each admitted
  `agent` effect request digest to at most one deterministic managed child
  task and its conclusive settled result. `request_digest` is the digest of
  the exact `algal.effect.v1` request the interpreter issued.
- **Wake discipline.** Each daemon owns two host mailboxes:
  `daemon-<name>-wake` and `daemon-<name>-inbox`. The wake mailbox carries
  only host nudges (`xcb.daemon-wake.v1` envelopes whose idempotency key is
  the canonical digest of the settled request digest); the program never
  receives from it. A suspended `agent` effect names the wake receive
  capability. When its linked child settles, the supervisor records the
  result row first, then sends the nudge — re-settlement replays the same
  delivery, never a duplicate. When the executor returns the settled output
  it drains the wake mailbox; nudges exist only for settled requests, so
  draining can never strand a live suspension.
- **Application mailbox.** The inbox carries operator messages
  (`xcb daemons send`). A program `mailbox.receive.v1` on an empty inbox
  suspends with the inbox capability, so a daemon that only waits consumes
  no generations, tokens or provider time.
- **Suspension and resume.** The registered `agent` host executor never runs
  work inline. On an unknown request digest it records intent and returns
  `EFFECT_SUSPENDED` with the wake capability; on a settled digest it returns
  the recorded result and the effect completes in that generation. ALGAL
  verifies the completed effect prefix on every dispatch; a replayed run
  cannot consume another model call or issue a second child for one request.
- **Child custody.** Every daemon child is an ordinary managed task in the
  daemon's conversation, created with deterministic identity derived from
  `xcb-daemon-child-v1\0<process>\0<request-digest>`. It consumes the current
  project grant exactly like a program child: grant generation, expiry,
  budget, pause, provider constraint and cancellation are rechecked at
  publication and dispatch. Child questions and approvals stay in the
  existing attention flow; the daemon cannot answer them.
- **Supervisor pump.** The managed daemon loop ticks processes alongside
  schedules: `ready` records dispatch with cause `start`; `suspended`
  records dispatch only when a wake capability is genuinely pending (nudge
  posted or inbox message present).
  `uncertain` records are never auto-retried — they surface as attention and
  recover only through the algal journal's exact-intent path. Terminal
  records are inert history.
- **Bounds.** ≤128 daemons per store, ≤64 generations, ≤8 agent calls per
  process, ≤32 cells, ≤128 edges, ≤64 steps, ≤16KiB outputs, ≤64KiB
  manifests, ≤32KiB interface inputs, ≤8KiB prompts, ≤8KiB summaries.
  Wake mailboxes hold ≤64 pending nudges; the inbox holds ≤64 messages of
  ≤8KiB each. Supervisor passes are bounded by the same fuel/tick limits as
  schedules. Oversized output is rejected, never silently clipped.
- **Retention and evidence.** Live daemons, their open child links, wake and
  inbox content are protected from retention sweeps. Every generation retains
  its algal receipt; `daemon_calls` rows retain child identity and settled
  result digests. Restart reconciles the record chain, the journal and the
  side table before any dispatch.

## Failure semantics

- A crashed supervisor leaves at most one `uncertain` intent; the exact
  intent digest gates journal recovery, and no child is created twice for
  one request digest.
- A daemon whose grant lapses holds: pending children cancel through
  ordinary cancellation; no new child is admitted until the grant returns.
- Mailbox overflow, malformed manifests, foreign process directories and
  interrupted creation markers all fail closed inside the existing algal
  contracts; xcb adds no second store.

## CLI surface

```sh
xcb daemons run <conversation> <name> <manifest> [--inputs FILE] [--calls N] [--generations N]
xcb daemons                        # list state, generation, pending wake
xcb daemons inspect <name>         # record chain, receipts, linked children
xcb daemons send <name> <message>  # post to the daemon inbox (bounded text)
xcb daemons stop <name>            # stop future dispatch; children settle
xcb daemons journal <name>         # intent digest and journal of an uncertain record
xcb daemons recover <name> --intent <digest>   # resume only that exact intent
```

Attention from a daemon's children lands in the existing `/attention` view.

## Delivery graph and ownership

1. Contract freeze — this document: store layout, admission profile, wake
   discipline, child custody, pump rules, retention and CLI surface.
2. `managed_daemon.rs`: `AdmittedDaemon` admission, `DaemonService`
   lifecycle (create/list/inspect/send/stop), the `agent` executor bridge
   and the `daemon_calls` table, colocated unit tests.
3. Supervisor pump: tick pass, wake gating, nudge delivery, uncertain
   surfacing, idle-loop integration so daemons keep the supervisor alive
   only while work is possible.
4. CLI surface and docs; example manifest under `examples/`.
5. Join review: crash mid-dispatch, duplicate settle, grant expiry
   mid-wait, cancellation propagation, retention protection, restart
   replay, generation budget exhaustion.
6. Required gates, isolated operator acceptance, release.

## Acceptance and progress

- [x] Contract frozen.
- [x] Admission rejects every disallowed cell kind, cap port, budget and
      oversize, and pins manifest/args digests.
- [x] A suspended daemon owns no slot and survives daemon restart.
- [x] One agent request yields exactly one managed child across retries,
      restarts and duplicate settle evidence.
- [x] Wake gating never burns a generation while a request is unsettled;
      drained nudges cannot strand a live suspension.
- [x] Uncertain records never auto-retry; journal recovery requires the
      exact current intent.
- [x] Generation/call/output bounds hold; replay consumes no model call.
- [ ] Live operator acceptance and release.
