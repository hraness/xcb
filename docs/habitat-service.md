# Habitat startup

Scheduling is owned by xcb and persisted in its state database. The operating
system only starts the supervisor. On macOS, `xcb service install` registers an
opt-in LaunchAgent for the exact `--state` root. It starts at desktop login and
checks once a minute after exit. It never installs as part of an upgrade.

```sh
xcb service plan
xcb service install
xcb service status
```

The declaration uses an absolute binary and state path, argument arrays, and a
bounded restart interval. It does not run a shell or use a continuous restart
loop. Each state root has a distinct label. Existing supervisor locking prevents
duplicate dispatch, and replacing an executable makes the old supervisor drain
before another implementation takes over.

To uninstall, pause project grants and schedules, allow active work to settle,
then run `xcb service uninstall` once the supervisor is idle. Uninstall holds the
supervisor lock while unloading the exact service. It refuses to terminate active
workers or replace an edited/foreign declaration. A failed registration retains
the exact install intent and declaration so `service install` can retry.

This service runs while the user is logged in; it does not promise execution while
the Mac sleeps or is powered off. xcb coalesces missed schedule occurrences on its
next start. Linux users can supervise `xcb --state /absolute/private/root
managed-daemon` with their user service manager; this release does not install a
Linux service automatically.

The declaration follows Apple's [launchd job
contract](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html).
