# Unattended operation

Excalibur (xcb) can defer new managed workers when the host runs short of
resources and restart a stalled supervisor through its login service. Check
these controls on each machine before relying on it while away. Power,
network access, disk unlock, provider sign-ins, and recovery of uncertain work
remain separate requirements.

## Enable resource protection

On macOS or Linux, inspect current measurements before enabling protection:

```sh
xcb --json resources --workspace /absolute/project
xcb resources enable
xcb --json service status
```

`resources` observes memory pressure, physical memory, swap use, and available
disk space for xcb's state folder and the optional project directory. It
launches no provider, removes no files, and signals no processes. JSON output
includes a timestamp, missing-measurement errors, policy, and an assessment
of this current sample. The live supervisor evaluates sustained pressure
and recovery across samples, so a one-shot command is not its full history.

Protection starts disabled. Once enabled, the supervisor samples every 30
seconds. Collection runs outside the task loop, with at most one collection
in flight. If a filesystem stops answering, xcb stops starting workers once
measurements become stale; it keeps handling completion and cancellation.
The last snapshot is saved as `managed/host-resources.json` for diagnostics.

Default decisions are:

| Condition | Behavior |
| --- | --- |
| Disk available space at or below 24 GiB | Include a resource advisory at the next worker turn |
| A known volume reaches 8 GiB | Defer new workers using it |
| A paused volume recovers above 12 GiB | Require three healthy samples before resuming |
| A newly observed volume has 12 GiB or less | Start paused until recovery is established |
| Memory warning or critical pressure lasts at least 60 seconds | Defer new workers until three normal samples arrive |
| Swap grows at least 1 GiB within the observed ten-minute window | Include an advisory; growth alone does not stop work |
| Required measurements fail or are older than 120 seconds | Defer new workers until measurements recover |

Both the state volume and the selected workspace volume must have room. A
different project's low-volume condition does not pause a healthy project.
Sampling rotates through large workspace sets without accumulating an
unbounded list. Unmeasured workspaces wait for their sample.

An advisory is host telemetry and grants no additional permissions. It is
included at a worker's next turn; it does not interrupt a provider already
generating a response. xcb does not automatically kill a large process or
delete a cache in response to these measurements.

Edit the `resources` section of the private `config.json` to adjust thresholds.
The supervisor reloads it every five seconds. Thresholds must preserve
`0 < pause_disk_bytes < resume_disk_bytes <= warn_disk_bytes`. Choose a reserve
that covers the largest expected job and the repository's required floor;
the default is a throughput-oriented 8 GiB pause / 12 GiB resume / 24 GiB warning band, not an allocation estimate for every project.

```sh
xcb resources disable
```

Disabling protection saves the setting without waiting for another measurement.
Monitoring is available through `xcb resources` even while protection is off.

## Keep unattended schedules moving

A schedule normally waits when a task in its directory needs an answer, even
if that task belongs to a linked program. For a project that should continue
without a person, opt in when creating a prompt or program schedule, or edit
an existing schedule using its current revision:

```sh
xcb schedules add <project> "Review previous work" --every 3600 --settle-unanswered-after 7200
xcb schedules program <project> planner.json --every 3600 --settle-unanswered-after 7200
xcb schedules edit <schedule-id> --revision <current-revision> --settle-unanswered-after 7200
```

After 7,200 seconds without an answer, xcb marks the unanswered task failed
with a saved reason and lets its waiting program settle; it does not retry the
task. This applies to every non-deferred task waiting for input in that directory,
not just work started by this schedule: approval and action requests, routing
questions, and tasks paused by an exhausted continuation budget can also fail.
Choose the timeout only if that is acceptable for the whole directory. A reply
starts a new wait if the task asks again. Running workers and uncertain outcomes
stay untouched. The option is off by default; edit with
`--settle-unanswered-after 0` to turn it off. Use `xcb schedules show <id>`
or `xcb --json schedules show <id>` to inspect the saved setting (milliseconds
in JSON). Pick an interval between 60 seconds and 365 days, and make sure
the schedule prompt checks earlier failures before starting new work. This
setting does not grant authority or answer permission requests.

## Investigate and reclaim resources

Use the [host maintenance runner](unattended-maintenance.md) for a periodic
agent review with host tools. An ordinary xcb coding worker is confined to its
workspace and cannot inspect or clean host caches. The maintenance runner
retains the host agent's configured permission and approval controls.

Investigations should identify the task responsible for growth, warn it with
the measured facts, and request cancellation through xcb's revision-checked
task command if needed. A cancellation request does not prove a process has
stopped. Keep account locks and uncertain task records until xcb confirms the
outcome.

For cleanup, verify each path again immediately before acting: it must be
reproducible, outside protected application and provider state, and free of
live use. Coordinate with the repository and host scheduler. A size report,
an old modification time, or an empty result from a failed process scan is
not proof that a path is disposable. Measure actual free space after every
cleanup. Preserve databases, sessions, credentials, evidence, dirty worktrees,
and unmerged work.

A provider session that dies mid-lane leaves exactly those artifacts:
uncommitted changes, unpushed commits, or a suspended merge or rebase, in a
project directory no live task claims. `xcb workspaces audit` probes every
registered directory for that shape and flags each row `STRANDED` once it has
sat past `--stale-hours` (24 by default); `--stranded-only` prints just the
paths for a host runner or a scheduled preservation task, and `--json`
reports the per-workspace branch, ahead count, change counts, and suspended
operation. The probe is read-only and never holds a lock.

Do not create a fresh xcb task every minute for telemetry. The task store has
a 4,096-task limit and a 30-day retention horizon; even one ten-minute agent
schedule could exhaust it. The host runner samples without model calls and
resumes one review context on its configured cadence.

## Verify recovery before leaving

Keep a record for each machine with its installed build, startup state,
measurements, and the results of these checks:

- Close the terminal and confirm the login service keeps running.
- In an isolated state folder, stop or stall an owned supervisor and confirm
  restart and truthful health reporting. Verify uncertain work is preserved.
- Simulate pressure in tests; confirm launches pause, cancellations finish,
  and recovery requires new healthy samples. Do not fill the real disk or
  stress the real machine to test thresholds.
- Disconnect and reconnect networking; verify relay recovery and sign-in
  refresh. Exercise the actual remote control and alert path.
- Take a consistent backup using the database's supported backup mechanism,
  protect credentials, and restore it into an isolated test environment.
  Copying a live SQLite file without its WAL is not a consistent backup.
- Run a several-day soak test and inspect growth rates, held accounts,
  unresolved tasks, review history, and remaining disk reserve.

The maintenance runner can send an optional five-minute heartbeat to the
existing xcb cloud backend. A separately hosted page, such as
`https://hraness.com/status`, can show a missed heartbeat even when the home's
power or internet is down. It retains only the latest receipt and reported
health for each generic laptop alias. This supplies an external status view;
it sends no notifications. See the runner guide for configuration and limits.

## Restarting a Mac

The login service runs in the user's session. It cannot unlock FileVault or
start a graphical login session. Amphetamine or another sleep assertion
also needs an actual running session; verify the power assertions on the
machine that will be left at home.

Apple documents [FileVault unlock over SSH after restart on Apple silicon
with macOS 26 or later](https://support.apple.com/guide/security/managing-filevault-sec8447f5049/web)
when Remote Login is enabled and networking is available. Test the complete
restart, remote unlock, user-session, and xcb-startup sequence while someone
is present. Do not assume a VPN app inside the locked session provides a
network path before unlock. Cold boot after exhausted power needs its own test.

Keep a physical recovery option for failures that remote software cannot fix.
These controls do not change FileVault, automatic login, power settings, or
operating-system update policy.
