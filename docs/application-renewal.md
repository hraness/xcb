# Application qualification renewal

`scripts/application-renewal.py` is an explicit local macOS installer and runner
for one previously qualified Claude account/model. It runs the existing fresh
prerequisite collector and the native fixed live challenge. It never changes the
24-hour qualification lifetime, imports an old capture as new evidence, grants
messaging permission, or repairs native account custody.

This helper is intended for a stable installed deployment. Keep its exact source
checkout, native binary, provider binary, Cargo, Node and Bun runtimes, collector, kernel
probe, and scheduler available and unchanged. An active development checkout
will stop renewal as soon as native source changes. Choose a frozen deployment
checkout before building and qualifying the final executable; copying a checkout
after qualification does not prove the new build is byte-identical.

## Explicit setup

First complete the [supported application qualification](../qualification/README.md#application-qualification-prerequisites)
on the exact deployment. The selected account must be enabled, connected,
admitted, idle, and currently qualified for the selected full model key. The
helper reads the existing nonsecret `application-generation.json` identity; it
does not copy or inspect credentials. It additionally requires the reviewed
native `--expected-generation` qualification guard described below;
binding and installation fail closed on older binaries. Paths below are
examples, not defaults.
Use physical paths for executables and source; the installed public scheduler
path may be a symlink, whose exact target is separately pinned. The Python
interpreter used for binding must also be a physical, single-link executable.
Apple’s bundled `/usr/bin/python3` may have multiple hard links and is refused;
select a physical Homebrew Python executable instead. Use that same interpreter
for binding, the first run, installation, and removal.

```sh
/absolute/physical/path/to/python3 -I /absolute/xcb/scripts/application-renewal.py bind \
  --directory /absolute/private/xcb-claude-renewal \
  --source /absolute/xcb --state /absolute/private/xcb-state \
  --xcb /absolute/path/to/installed/xcb \
  --provider-executable /absolute/path/to/native/claude \
  --scheduler /absolute/path/to/hra-host-run \
  --cargo /absolute/path/to/physical/cargo \
  --node /absolute/path/to/physical/node --bun /absolute/path/to/physical/bun \
  --account a_selected --model claude/sonnet/low
```

Binding creates a new mode0700 directory and mode0600 files. It performs only
read-only native inspection/capability commands and Git/toolchain identification;
it does not refresh an account, run Cargo gates, contact a provider, or schedule
anything. Existing output directories are refused. Python3.9+, the macOS Rust
installation under `/opt/homebrew`, and the HRA host scheduler are required by
this initial implementation.

After review and coordination with the native account owner, install explicitly:

```sh
/absolute/physical/path/to/python3 -I /absolute/xcb/scripts/application-renewal.py install \
  --directory /absolute/private/xcb-claude-renewal
```

This publishes one exact owned LaunchAgent in `~/Library/LaunchAgents` and
bootstraps it in the current user's GUI domain. Installation verifies current
pins and qualification again. `RunAtLoad` is false: installation launches no
provider request. Launchd checks hourly while the user is logged in; it does not
wake a sleeping Mac, renew while logged out, or promise network availability.
An explicit `run --directory ...` uses exactly the same runner when needed.
Invoke `run` directly: it schedules its own host phases, so an outer scheduler
lease can block its exclusive collection phase.
The integration owner may use `run --directory ... --renew-now` for the first
coordinated live acceptance. This explicitly starts fresh collection even when
the existing receipt has more than12hours remaining; every source, account,
custody, evidence, and expiry gate still applies. The LaunchAgent never passes
this option.

## What a run does

1. Acquire a nonblocking private flock on a persistent inode. A second renewal
   owner cannot run in parallel. Recheck source, binaries, toolchain, controlled
   environment, ambient Cargo configuration, and the bound account generation.
2. Inspect current native capabilities without launching a provider. Defer a
   busy native account. If the existing qualification has more than12hours left,
   record its existing expiry and finish without renewing it.
3. Record a durable attempt intent. Refresh the selected account through the
   supported `xcb accounts refresh ID`, then recheck all pins and native context.
   Refresh supplies current model observations; it is not qualification.
4. Invoke the existing collector in a new private directory through the exact
   installed scheduler, with `--mode=exclusive --lane=mac-native`. It runs fresh
   Cargo formatting, workspace tests, Clippy, a release build, and the native
   Claude kernel probe. The release artifact must equal the installed binary.
5. Recheck pins and run the separate fixed qualification challenge, passing the
   original `--expected-generation`, through
   `--mode=shared --lane=mac-native`. The native executor owns account leases,
   bounded model execution, join evidence, publication, and credential custody.
   The helper does not create another native lease or release a busy one.
6. Independently read capabilities and the resulting private receipt. Require
   the receipt's exact digest, generation/model, an observation from this new
   attempt, and the native expiry of at most24hours from that observation.
   This final read-only check may accept `account_busy` when a normal application
   turn started after the challenge completed, provided the account remains
   enabled, connected, admitted, and qualified for the selected model. Binding,
   installation, and prelaunch checks still require an idle account. Preserve
   the receipt's actual expiry and clear only this helper's completed intent.

Each provider operation occurs at most once in a run. Native account custody is
authoritative; a busy check is only a scheduling hint, not permission to launch.
If another owner wins the native lease after the hint, the native command fails
closed. This version retains that unsuccessful attempt for review rather than
looping or guessing that retry is safe.

## Identity limitation and activation gate

The native qualifier accepts `--expected-generation`; this helper passes the
pinned value on every challenge. Native XCB reads the existing generation under
its exclusive account lease **before the live challenge**, refuses missing or
mismatched generations without creating a new one, and checks the generation
again before publishing evidence. This prevents a concurrent explicit sign-in
replacement from qualifying a different generation on the helper's behalf.

Older binaries without this guard are refused. CLI help discovery is a version
compatibility check; its safety depends on the exact pinned native implementation
having passed review and fresh qualification. Neither the helper nor its tests
claim unattended acceptance on an older binary. The integration owner must
coordinate the first live renewal, validate the final installed bytes, and record
the actual result.

## Failure, status, and removal

`last-status.json` is one bounded, body-free status record. Completed and failed
attempts retain bounded private command intents, outputs, exit records, and the
collector's evidence. A source/runtime/provider/configuration/account change
stops the job and requires explicit rebinding after normal qualification. There
is no automatic adoption of upgrades or replacement sign-ins.

A command deadline, failed command, interruption, missing successful receipt,
or uncertain join leaves `pending.json` in place. Future runs stop before any
provider operation. Inspect the exact attempt and use XCB's documented custody
diagnostics; never infer recovery from an expired lease, missing PID, or elapsed
time. This helper deliberately provides no command to clear uncertainty. A new
binding is not a native recovery mechanism and cannot bypass a held account.

Evidence is not automatically deleted. The helper refuses new attempts after64
attempt directories,1GiB of retained evidence, or4096files. Cargo's own build
outputs remain subject to normal repository/host disk policy. Raw outputs are
bounded to4MiB per command, and each collection uses the collector's additional
per-artifact bounds. Retain qualification evidence during any disk cleanup.

```sh
/absolute/physical/path/to/python3 -I /absolute/xcb/scripts/application-renewal.py uninstall \
  --directory /absolute/private/xcb-claude-renewal
```

Uninstall requires an exact matching ownership receipt and plist and a successful
native `launchctl bootout`. It will not remove a changed/foreign job. Binding,
attempts, pending intent, account state, and qualification receipts are preserved.
If bootstrap or bootout returns an uncertain result, inspect the retained job;
do not delete launchd or scheduler state as a workaround.

The controlled PATH selects the explicitly pinned physical Node and Bun executables
needed by native interoperability tests; shell startup and NVM initialization are
not required. Runtime replacement or a shadowing executable stops renewal.
The collector also fingerprints the Git-visible TypeScript source used by native
interoperability tests, root package manifests and lockfiles, TypeScript/Bun
configuration, and the resolved Node/Bun executable bytes and bounded version
output. Changes to these inputs require fresh evidence and explicit rebinding,
even when the installed native binary remains unchanged.
The controlled environment includes explicit HOME/PATH/CARGO_HOME, fixed
locale settings, and `CARGO_INCREMENTAL=0` to avoid accumulating incremental
compiler caches during repeated evidence collection. The collector includes all
`CARGO_*` variables in its source/toolchain fingerprint, so this setting is part
of the bound evidence identity. A binding or capture made under a different
environment requires explicit rebinding and fresh evidence; it is not silently
adopted. The release artifact must still match the installed binary exactly.
The helper does not inherit provider keys, proxy settings, Rust compiler
overrides, or shell startup commands. The scheduler's public absolute invocation
and complete child argv are preserved in each private intent; no shell is used.

Run synthetic tests under the host scheduler. They never call native XCB, Cargo,
launchctl, or a provider:

```sh
/absolute/path/to/hra-host-run --mode=shared --lane=mac-native --label=xcb-renewal-synthetic-tests -- \
  /usr/bin/python3 -I /absolute/xcb/scripts/application-renewal.test.py
```
