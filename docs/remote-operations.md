# Remote operations

How the xcb fleet runs day to day: enrollment, controller use (human or
agent), recovery, and the deployment record. The contract lives in
`docs/plans/remote-access.md`; this file is the runbook.

## Deployment record

| Item | Value |
| --- | --- |
| Project | `cclrte:xcb` (Convex team `cclrte`) |
| Production deployment | `prod:terrific-rook-891` |
| Relay URL | `https://terrific-rook-891.convex.cloud` |
| Site/auth URL | `https://terrific-rook-891.convex.site` |
| Dev deployment | `dev:neat-seal-397` |

Deploy code changes from the repo root:

```sh
CONVEX_DEPLOYMENT=dev:neat-seal-397 npx convex deploy
```

Deployment env (`npx convex env set <KEY> <value>` with
`CONVEX_DEPLOYMENT=prod:terrific-rook-891`):

- `JWT_PRIVATE_KEY`, `JWKS` — the RS256 pair that mints and verifies
  device sessions. Rotate by replacing both.
- `XCB_RELAY_BOOTSTRAP` — one-shot first-owner invite. Inert once a
  subject is verified; set a fresh value only when rebuilding a fleet.
- `XCB_RELAY_EMAIL` — `log` (default), `resend`, or `webhook`.
  `resend` needs `XCB_RESEND_API_KEY` and `XCB_RESEND_FROM`; `webhook`
  needs `XCB_OTP_WEBHOOK_URL` and `XCB_OTP_WEBHOOK_TOKEN`.

OTP delivery currently uses `log`: the sign-in code is printed to the
function log and read from an authenticated Convex session:

```sh
CONVEX_DEPLOYMENT=prod:terrific-rook-891 npx convex logs
```

A `convex logs` tail is needed once per `xcb link`. Switching to Resend
is one env triple — no code change — when real email delivery is wanted.

## Enrolling a laptop

On the laptop, from the repo (or an installed `xcb`):

```sh
xcb link --relay https://terrific-rook-891.convex.cloud \
  --email <owner email> --label <machine name>
# read the OTP from `npx convex logs` on the prod deployment
xcb link --relay https://terrific-rook-891.convex.cloud \
  --email <owner email> --code <8-digit code> --label <machine name>
```

The first link of the day prints a device id ending with `waiting for an
enrolled device to run xcb remote admit <device>` whenever another
device already holds the account key. Admit from any enrolled machine:

```sh
xcb remote admit <device>
```

Then keep the supervisor resident — it serves remote commands and
publishes the fleet projection. On macOS the repo's own LaunchAgent does
this for the exact state root (see `docs/habitat-service.md`):

```sh
xcb service plan      # inspect the declaration
xcb service install   # register; restarts a minute after exit
xcb service status
```

A bare `xcb managed-daemon` foregrounds the same supervisor for a
one-off run.

A `xcb link` rerun on an already-linked machine is a no-op. Retried
enrollment reuses the persisted device identity; a session bound to a
device that no longer exists locally fails closed rather than rebinding.

## Controller contract (agents: Grok or otherwise)

Every verb is scriptable: `--json` prints one JSON object on stdout,
diagnostics stay on stderr, and the only interactive step in the whole
surface is the `xcb link` code prompt. A controller agent holds a
`--state` directory with cloud custody (the default state root —
`~/.local/share/xcb` here — or a dedicated root) and runs:

```sh
xcb --state <root> fleet --json            # devices, presence, projection staleness
xcb --state <root> attention --remote --json
xcb --state <root> dispatch <device> <workspace> -p "<task>" --json
xcb --state <root> remote status <command-id> --wait --json
xcb --state <root> remote steer|cancel|answer|refresh …
xcb --state <root> remote abort|ack <command-id>
xcb --state <root> remote admit|revoke <device>
```

Loop contract for a driving agent:

1. `dispatch` returns `command` (public id) immediately.
2. `remote status <id> --wait` blocks until the lifecycle closes —
   exit `0` only on `applied`; `failed`, `ambiguous`, `cancelled` and
   `expired` all exit `1` with `resultCode` and decrypted `result`;
   an unknown id or an exhausted wait exits `2`.
3. `attention --remote` lists open attention items; `remote answer`
   responds; `remote steer`/`cancel` drive the task by id.
4. `remote abort` withdraws a command while still `pending`.
5. `remote ack` acknowledges a terminal command for retention.

`fleet --json` and `attention --remote --json` report each projection's
`updatedAt` and a `stale` flag. A lane republishes the projection
whenever contents change and touches it at least every ten minutes
otherwise, so `stale` (older than twenty minutes) means the lane stopped
writing — treat the body as "unknown, not empty" and check presence and
supervisor faults.

Commands are idempotent: retrying a dispatch with the same idempotency
key replays the in-flight command rather than double-executing.
`ambiguous` means the effect may have begun before the executor lost
track of it — reconcile against the remote machine, never blindly retry.

## Recovery

- **Lost or retired laptop**: `xcb remote revoke <device>` from any
  remaining device. The id never rebinds; in-flight commands settle or
  expire under the new auth epoch and the device's lane is auth-fatal
  within one poll.
- **Expired session**: refreshed automatically before every call;
  custody persists the new session. A laptop offline past the refresh
  window re-auths cleanly on its next boot — no manual step.
- **Daemon crash or restart**: the supervisor reboots the lane under a
  bumped boot generation; stale claimants are fenced by authority and
  close honestly (`failed`/`ambiguous`). Reboots back off to 5 minutes
  on consecutive failures.
- **Relay unreachable**: every call is bounded at 30s; the lane retries
  on the backoff cadence and presence re-arms on reconnect.
- **Revoked custody**: `xcb link` re-enrolls a fresh device identity;
  the old id stays retired.

## Scratch roots on this machine

Two surrogate state roots exist for prod testing and can be revoked at
any time from any enrolled device:

- `/Users/benguo/xcb-prod-a` — device `832a7d26…` (the first owner
  device; it minted the account key)
- `/Users/benguo/xcb-prod-b` — device `513c79af…`

The real custody lives at `~/.local/share/xcb` (device `f07e6b26…`,
label `HRA2`). HRA2 runs the supervised daily-driver setup: release
binary at `~/.local/bin/xcb`, LaunchAgent
`dev.hraness.xcb.habitat.6fb2ab5e88dce28b9cd3667b` installed via
`xcb service install`, online on the production relay.
