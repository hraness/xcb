# Login startup

xcb's background supervisor runs your tasks, schedules, and project work, and
keeps running after you close the terminal. xcb starts it when needed. On macOS,
`xcb service install` also starts it when you log in, for the state folder you
name with `--state` (or the default). It checks once a minute after the
supervisor exits and restarts it. Installing or upgrading xcb never turns this
on.

```sh
xcb service plan      # print the launchd declaration without installing it
xcb service install
xcb service status    # whether it starts at login, whether the supervisor runs, and its log
```

The declaration uses absolute paths for the binary and state folder, runs no
shell, and restarts at most once a minute. Each state folder gets its own label,
`dev.hraness.xcb.habitat.<id>`, and its log is in `~/Library/Logs/xcb`. The
supervisor's lock prevents two supervisors from running the same work, and a
replaced binary's supervisor finishes its running tasks before the new one
takes over.

When the supervisor runs at login, macOS may block it from folders such as
Documents, Desktop, or Downloads. `xcb service status` reports the blocked
folder and the System Settings pane to change.

To remove it, pause project grants and schedules, let active work finish, then
run `xcb service uninstall` once the supervisor is idle. Uninstall refuses to
stop active workers or to replace a declaration that was edited by hand. If
registration fails, running `xcb service install` again retries it.

The service runs while you are logged in; it doesn't run while the Mac sleeps or
is off. xcb runs missed schedule occurrences once on its next start. On Linux,
run `xcb --state /absolute/private/root managed-daemon` from your user service
manager; xcb doesn't install a Linux service.

The declaration follows Apple's [launchd job
contract](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html).
