# Login startup

xcb's background supervisor runs your tasks, schedules, and project work, and
keeps running after you close the terminal. xcb starts it when needed. On macOS
and Linux, `xcb service install` also starts it when you log in, for the state
folder you name with `--state` (or the default). It checks once a minute after the
supervisor exits and restarts it. Installing or upgrading xcb never turns this
on.

```sh
xcb service plan      # print the launchd or systemd declaration without installing it
xcb service install
xcb service status    # whether it starts at login, whether the supervisor runs, and its log
```

The declaration uses absolute paths for the binary and state folder, runs no
shell, and restarts at most once a minute. Each state folder gets its own label,
`dev.hraness.xcb.habitat.<id>`. A watchdog checks the supervisor it starts and
keeps three diagnostic log files of at most 2 MiB each under
`<state>/managed/service-logs`. The
supervisor's lock prevents two supervisors from running the same work, and a
replaced binary's supervisor finishes its running tasks before the new one
takes over.

`xcb service status --json` reports `supervisor_health` as `fresh`, `stale`,
`missing`, or `stopped`, together with the heartbeat age. A held process lock
alone does not establish that the supervisor is responding. The watchdog
allows startup and wake-from-sleep time before acting on a stale heartbeat.
It can stop only the child it launched and verified. It does not kill worker
process groups, clear account locks, or retry work whose outcome is uncertain.

Older service declarations remain supported and appear with
`watchdog_enabled: false`. To adopt the watchdog, let work finish, pause future
work, uninstall the idle service, and install it again. An edited service file
is preserved. Check the reported health and watchdog state after installation.
`supervisor_watched` describes the running supervisor; an installed watchdog
cannot adopt a supervisor that was already started separately. Once that
supervisor exits safely, new starts use the installed watchdog.

When the supervisor runs at login, macOS may block it from folders such as
Documents, Desktop, or Downloads. `xcb service status` reports the blocked
folder and the System Settings pane to change.

To remove it, pause project grants and schedules, let active work finish, then
run `xcb service uninstall` once the supervisor is idle. Uninstall refuses to
stop active workers or to replace a declaration that was edited by hand. If
registration fails, running `xcb service install` again retries it.

The service runs while you are logged in; it doesn't run while the Mac sleeps or
is off. xcb runs missed schedule occurrences once on its next start.

## Linux

On Linux the declaration is a systemd user unit,
`~/.config/systemd/user/xcb-habitat-<id>.service`, which xcb enables with
`systemctl --user enable --now`. systemd restarts the supervisor a minute after
it exits. The watchdog keeps its logs under the state folder. Older service
declarations write to `~/.local/state/xcb`. It needs a systemd user
manager, which a desktop or SSH login session has.

systemd stops your user services when your last session ends. On a server or
other host you reach over SSH, keep the supervisor running after you log out
with:

```sh
loginctl enable-linger "$USER"
```

`xcb service install` reminds you when lingering is off. `xcb update enable`
writes `xcb-update.timer` and `xcb-update.service` beside the supervisor unit
for the daily update check. xcb replaces or removes only units it wrote; a unit
with the same name that you wrote stays untouched, and the command refuses.

The declaration follows Apple's [launchd job
contract](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html).

For memory and disk protection, see [unattended operation](unattended.md).
