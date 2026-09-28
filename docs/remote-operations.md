# Remote operations

`xcb link` joins machines into a fleet, so you, or an agent acting for you, can
see their work and start tasks on them from any linked machine with the same
CLI. There is no web interface: you enroll with an emailed code in the
terminal, and task content crosses the relay end-to-end encrypted.

The fleet needs a relay: a [Convex](https://convex.dev) deployment of this
repository's `convex/` backend that you run. `xcb link --relay <url>` or
`XCB_RELAY_URL` points xcb at it. The first link on a machine needs one of
them; xcb saves the relay at enrollment and later commands use it.
[Relay deployment](relay-deployment.md) lists the settings a relay needs.

## Enroll a machine

Install xcb on the machine, then link it:

```sh
xcb link --relay https://<your-deployment>.convex.cloud \
  --email <owner email> --label <machine name>
# The 8-digit code arrives by email; finish with --code:
xcb link --relay https://<your-deployment>.convex.cloud \
  --email <owner email> --code <8-digit code> --label <machine name>
```

When another enrolled device already holds the fleet's key, the new device
waits for approval and prints its ID. Approve it from any enrolled machine:

```sh
xcb remote admit <device>
```

Then keep the background supervisor running, because it serves remote commands
and publishes the machine's status. On macOS, `xcb service install` starts it at
login for this state folder (see [login startup](habitat-service.md)). Running
`xcb link` again on a linked machine changes nothing, and a retried enrollment
reuses the saved device identity.

## Renew a relay sign-in

If xcb cannot refresh a linked machine's expired sign-in, run:

```sh
xcb link --reauth --email you@example.com
# Enter the emailed code at the terminal prompt, or finish with:
xcb link --reauth --email you@example.com --code <8-digit-code>
```

Use the same account and state folder that linked this machine. Renewal keeps
the device ID, device keys, account key, and local tasks. It uses the saved relay;
any `--relay` or `XCB_RELAY_URL` override must match that address. `--controller`,
`--invite`, and `--label` are enrollment options and cannot accompany `--reauth`.

The relay finishes its current operation before switching sign-ins. Local
provider tasks continue running. After an upgrade, an older background
supervisor may need to finish its active tasks and exit before renewal can
proceed. If xcb asks you to wait, let those tasks finish and repeat the command.

If the connection drops or the command stops before reporting success, rerun
`xcb link --reauth` with the same state folder and omit `--email` and `--code`.
xcb reports a completed renewal while its saved sign-in is current, or
continues an unfinished renewal before requesting another code. To start a
new renewal after a successful one, supply `--email` again.

If a pending sign-in has expired completely after seven days, xcb asks for a
new emailed code. Remote work stays paused until renewal finishes; local
work can continue.

Outside a terminal, supply `--email`; the first invocation emails a code and
asks you to repeat with `--code`. Supplying `--code` verifies that code without
emailing a replacement. `--json` reports successful renewal with
`"reauthenticated": true` and never includes tokens or keys.

On a workspace machine, xcb also checks that its background supervisor can
start. If startup fails after sign-in succeeds, the command reports the saved
sign-in and a separate startup warning. Run `xcb chat` with the same state
folder to retry startup. A controller device does not start a supervisor.

## Drive the fleet

```sh
xcb fleet                                          # devices, presence, and how fresh each status is
xcb attention --remote                             # questions and approvals across machines
xcb dispatch <device> <workspace> -p "<task>"      # start a task on another machine
xcb remote status <command-id> --wait              # wait for a command to finish
xcb remote steer|cancel|answer <device> <task>     # drive a remote task
xcb remote abort|ack <command-id>                  # withdraw or acknowledge a command
xcb remote admit|revoke <device>                   # approve or retire a device
```

IDs may be typed as unambiguous prefixes, such as `xcb remote cancel 513c t_a65b`.
`<workspace>` is one of:

- an absolute path on the target machine, used exactly as given;
- a project name that matches exactly one project registered there (see
  `xcb workspaces` on that machine); send names only to devices whose
  `capabilities` include `workspace-names`;
- `@infer`, which lets the device pick from a registered path or unique project
  name in the prompt, or a “continue” of the last remote task within six hours.
  It never falls back to a recent folder. Send it only to devices whose
  `capabilities` include `infer`.

The home folder, `/`, hidden folders in your home, `~/Library`, xcb's own
folders, and system folders are refused. A dispatch lands in the target
machine's thread and returns the task ID, the folder, and how it was chosen:
`{"conversation":"c_global","dispatched":true,"task":"t_…","workspace":"/abs","workspaceSource":"explicit"}`.

## Agents as controllers

Give each agent its own controller device and private state folder. A state
folder contains the device identity and account key, so access to it grants
that device's authority.

```sh
xcb --state ~/.local/share/xcb-agent link --controller \
  --relay https://<your-deployment>.convex.cloud --email <owner email> --label agent
xcb remote admit <device-id>      # from any enrolled machine
```

Point the agent at `xcb --state ~/.local/share/xcb-agent --json <command>`. A
controller device reads fleet status and posts commands; nothing can be
dispatched to it. Every command prints one JSON object on stdout with `--json`,
diagnostics go to stderr, and the only interactive step is the `xcb link` code
prompt.

Your CLI and background supervisor coordinate sign-in refresh when they use
the same state folder. Upgrade both before using them together; replacing the
CLI executable does not immediately upgrade a supervisor that is finishing
active work. Each process reloads a session saved by another process before
requesting a refresh. A changed device, account, or relay stops the operation
and asks you to rerun it with the current state. This coordination does not
make a device's private state folder suitable for sharing with agents.

An interrupted enrollment can resume without its device or account key when
the saved sign-in identifies its relay. An older incomplete enrollment with
no saved relay must sign in again through `xcb link`; xcb does not send that
stored refresh token to an unconfirmed destination.

A driving agent's loop:

1. `dispatch` returns the command ID at once.
2. `remote status <id> --wait` blocks until the command finishes. It exits `0`
   only when the command was `applied`; `failed`, `ambiguous`, `cancelled`, and
   `expired` exit `1` with `resultCode` and the decrypted `result`; an unknown
   ID or an exhausted wait exits `2`.
3. `attention --remote` lists open questions and approvals; `remote answer`
   responds, and `remote steer` or `remote cancel` drive a task by ID.
4. `remote abort` withdraws a command that is still `pending`.
5. `remote ack` acknowledges a finished command so it can be cleaned up.

`fleet --json` and `attention --remote --json` report each machine's
`updatedAt` and a `stale` flag. A machine republishes its status whenever it
changes and at least every ten minutes, so `stale` (older than twenty minutes)
means it stopped publishing: treat the status as unknown, not empty.

Commands are idempotent: retrying a dispatch with the same idempotency key
returns the command already in flight instead of running it twice. `ambiguous`
means the work may have started before the machine lost track of it; check the
target machine before retrying.

## Recovery

- **Lost or retired machine:** run `xcb remote revoke <device>` from any other
  enrolled machine. The ID is retired for good; commands in flight finish or
  expire, and the revoked machine loses access on its next poll.
- **Expired session:** xcb attempts a refresh before an authenticated call.
  If refresh is rejected, [renew the relay sign-in](#renew-a-relay-sign-in)
  with `xcb link --reauth` using the same account.
- **Supervisor crash or restart:** the supervisor restarts its relay link, and
  commands left from before close as `failed` or `ambiguous`. Restarts back off
  up to five minutes after repeated failures.
- **Relay unreachable:** every call gives up after 30 seconds and retries on
  the same backoff; presence returns when the relay does.
- **Revoked device:** renewal cannot restore a revoked ID. Use a new private
  state folder with `xcb --state <new-folder> link` to enroll again, and keep
  the old folder if it contains local work you need.
