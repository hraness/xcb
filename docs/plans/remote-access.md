# Remote access: xcb machines as a fleet

## Outcome and scope

Treat every machine running xcb as an enrolled device in a Convex relay.
A person — or an agent acting for them — checks in on and dispatches work
to the fleet from anywhere, through the xcb CLI alone. There is no web
surface: enrollment is email-OTP in the terminal, and the controller is an
enrolled device class, not a hosted page.

This slice delivers the relay and the two device classes:

- **Daemon devices** are machines running xcb. Each publishes bounded,
  end-to-end encrypted projections of its managed state (workspaces,
  conversations, task and daemon summaries, open attention) and consumes
  device commands addressed to it under its own boot authority.
- **Controller devices** are enrolled `xcb` clients holding a wrapped copy
  of the owner's key. `xcb fleet`, `xcb dispatch`, `xcb attention --remote`
  and `xcb send` read projections and write commands through the relay.
  Any agent that can run a shell command — a hosted coding agent, a chat
  bot on an operator machine — is a controller.

Out of scope: a hosted HTTP/MCP surface for platforms that cannot run a
shell, per-controller authorization narrowing (controllers act as the
owner), and migration of hra/alt onto the shared relay.

## Shared foundation

The relay itself is a new repository, `hraness/relay`, extracted as a
product-neutral foundation so hra and alt can adopt it later:

- `convex/` — backend factories: verified-email auth subjects and OTP
  challenges, the device registry, capability-bound enrollment invites,
  the closed device-command lifecycle (`pending → prepared →
  effect_started → applied | failed | ambiguous | cancelled | expired`,
  fenced by the target daemon's boot authority), the envelope store, rate
  buckets, and retention crons. Each product instantiates these with its
  own wire namespace (`xcb.relay.*.v1`) and bounds.
- `wire/` — the contract specification and validators: envelope shapes,
  authority tuples (user + device + auth epoch + boot generation),
  idempotency keys, digest pinning.
- `crypto/` — the pinned end-to-end envelope scheme: ECDSA P-256 low-S
  signatures over canonical JSON, ECDH-P256 device pairing, AES-GCM-256
  payloads. Portable by construction — WebCrypto in TypeScript, the `p256`
  and `aes-gcm` crates in Rust.
- `client-ts/` — the typed TypeScript client used by TS-side products.

xcb implements its Rust client against `wire/` using the official `convex`
crate for transport; wire compatibility is the contract, not shared code.

## Contracts

- **Authority.** A verified-email subject is the only identity. A device
  credential is a P-256 keypair enrolled through a capability-bound invite
  or the owner's own OTP login. Every write revalidates user, device, auth
  epoch and — for daemon-addressed commands — the daemon's boot authority
  generation. Revocation retires the device id; in-flight commands settle
  or expire, never replay under a dead device.
- **Encryption.** The relay stores ciphertext envelopes and opaque
  metadata only: device ids, projection revisions, command states,
  bounded timestamps. Task text, prompts, outputs, workspace paths and
  attention content are encrypted to the owner's key before transport.
  Controller devices hold a wrapped copy of the key minted at enrollment.
- **Commands.** xcb device commands are a closed union:
  `task_dispatch`, `task_steer`, `task_cancel`, `attention_answer`,
  `daemon_send`, `projection_refresh`. Each carries a bounded encrypted
  payload, an idempotency key, and an expiry (~1 day). A daemon claims a
  command under its boot authority, executes it through the ordinary
  `ManagedStore` operation — never a parallel authority — and posts an
  encrypted result. An effect that may already have begun closes as
  `ambiguous`, not `applied`.
- **Projections.** A daemon publishes an encrypted fleet projection per
  workspace on change and on a bounded interval: device label,
  conversation/task ids, states, attention counts, daemon names and
  states, latest settled receipt digests. Content bytes stay inside
  envelopes; the projection row exposes only ids, revisions and counts.
- **Bounds.** Relay payloads ≤64KiB ciphertext; commands ≤8KiB plaintext
  before sealing; projections ≤32KiB plaintext; ≤64 pending commands per
  device; ≤64 enrolled devices per user. Enrollment invites are
  single-use and expire in minutes. OTP sends are rate-bucketed per
  address.
- **Custody.** Device keys live under `~/.xcb/cloud/` in a 0700 directory
  with 0600 files, matching `private.rs` conventions. The daemon never
  holds the user key longer than a seal operation needs it; provider
  credentials, session bodies and environment values never enter a
  projection or command payload.
- **Offline.** A device absent past its boot-authority TTL has its
  pending commands expire in place. Projections carry a `seenAt`
  timestamp; controllers surface staleness rather than silently reusing
  old state.
- **Link resumption.** An auth session binds exactly one device.
  `xcb link` persists the device identity before enrolling, so a retried
  link reuses it: an `active` row means enrollment already completed, a
  `pending` row resumes binding, an absent row registers fresh, and a
  session already bound to an unknown local device is a conflict — an
  orphaned active device bound to a dead session is revoked and a fresh
  identity enrolls, since device ids never rebind. `relay.json` and
  `account.json` mark a completed link; `device.json` alone marks a
  resumable one. Stored sessions refresh before any new OTP is
  requested, including while waiting on an account-key wrap.
- **Supervisor lane.** A linked machine's managed supervisor hosts the
  relay lane: it polls device-addressed commands on a fixed cadence even
  with no local task in flight, keeps the process resident instead of
  idle-exiting, and disconnects presence on shutdown. Boot failures are
  bounded supervisor faults; a revoked device is auth-fatal and stops
  the lane rather than retrying forever.

## Failure semantics

- A crashed daemon leaves commands it claimed as `ambiguous`; the next
  boot authority fences out the dead claimant and the command may be
  retried or expired by the controller's own re-issue.
- A controller that publishes a command and loses the reply reconciles
  by idempotency key, never by speculative re-dispatch.
- An envelope that fails to open, or a projection whose declared
  ciphertext digests do not match, is refused wholesale — no partial
  reads.
- Relay compromise exposes ciphertext plus metadata only. A stolen
  device key is revoked at the registry; commands in flight settle
  against the new auth epoch.
- Provider custody is unchanged: remote dispatch still lands as an
  ordinary managed task under the project grant, with the same
  admission, sandbox and attention surfaces as local work.

## CLI surface

```sh
xcb link [--email <address>]      # enroll this device (daemon or controller)
xcb fleet                          # devices, workspaces, states, staleness
xcb dispatch <device> <workspace>  # enqueue a managed task remotely
xcb attention --remote             # open attention across the fleet
xcb send <device> <daemon> <text>  # post to a remote daemon inbox
xcb remote admit <device>          # wrap the account key for a new device
xcb remote revoke <device>         # retire a lost or retired device
xcb remote steer <device> <task> <text>   # queue guidance on a remote task
xcb remote cancel <device> <task>         # cancel a remote managed task
xcb remote answer <device> <task> <text>  # answer remote attention
xcb remote refresh <device>        # republish the fleet projection now
xcb remote status <id> [--wait]    # lifecycle + result of a posted command
xcb remote abort <id>              # withdraw a still-pending command
xcb remote ack <id>                # acknowledge a terminal command
```

Every remote verb returns the posted command's public id; `xcb remote
status --wait` blocks until the lifecycle closes and exits 0 only on
`applied`, so a controller agent can drive the whole loop —
dispatch → wait → attention → answer — without holding a socket.

## Delivery graph and ownership

1. `hraness/relay`: foundation repo — auth, devices, invites, command
   lifecycle, envelopes, crypto, wire contract, local-backend tests.
2. `xcb/convex/`: the xcb deployment — instantiated schema, xcb command
   union, projection tables, deployment configuration.
3. `crates/xcb-runtime/src/cloud/`: device credential custody, `xcb
   link`, the supervisor relay lane (projection publisher + fenced
   command consumer), and the `p256`/`aes-gcm`/`convex` crate client.
4. Controller commands (`xcb fleet`, `dispatch`, `attention --remote`,
   `send`, `remote revoke`) and wrapped-key custody.
5. Join review: enrollment replay, command fencing under daemon restart,
   projection staleness, revocation mid-flight, offline expiry,
   envelope-tamper rejection, bound checks.
6. Live two-machine acceptance, docs, release.

## Acceptance and progress

- [x] Contract frozen.
- [x] `hraness/relay` foundation green: auth subjects, device registry,
      invites, fenced command lifecycle, envelope round-trips, retention.
- [x] xcb Convex deployment serves the instantiated schema; local
      backend dev loop works without credentials.
- [x] `xcb link` enrolls daemon and controller devices; revocation
      retires a device end to end.
- [x] A controller can observe the fleet and dispatch a task that lands
      as an ordinary managed task under the remote project's grant.
- [x] No plaintext task content is observable in the relay database.
- [x] Production Convex deployment (`prod:terrific-rook-891`) serves the
      schema end to end: OTP enrollment via `log` transport, bootstrap
      first-owner admission, device admit + key wrap, presence, fenced
      dispatch → managed task → `applied` settlement with decrypted
      result, and CAS-revisioned fleet projection — all verified live
      over the internet between two custody roots.
- [x] Controller surface covers the whole closed union plus the
      posted-command lifecycle: `remote steer|cancel|answer|refresh`,
      `status --wait` (exit 0 only on `applied`), `abort`, `ack`.
- [x] Supervised residency on a release binary: HRA2 and the prod-b
      surrogate both run the managed daemon under the repo's own
      LaunchAgents from the installed `v0.8.11` archive; presence
      re-arms on boot and the fleet projection stays fresh through the
      ten-minute touch. The same archive was smoke-tested for
      cryptographically-dead session recovery on prod custody (~50s
      forced refresh and retry instead of a permanent wedge).
- [ ] Live two-physical-laptop acceptance recorded (laptop 2 enrollment
      is a documented runbook step in `docs/remote-operations.md`).
