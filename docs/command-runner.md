# Isolated workspace commands

**Installed Claude and Codex coding workflows passed on macOS ARM64 with the
tested accounts and admitted builds.** Each ran an expected failing test, made
the exact repair, passed the test, and inspected filtered Git status, with joined
processes and settled effects. The backend also passed all 12 mandatory VM
boundary cases, including public dependency fetching, offline Cargo/Bun use from
immutable caches, and rejection of a cache after its manifest changed. These
results apply to the tested accounts, models and builds. Devin's credential-free
boundary qualification is separate from live coding acceptance; the dated
[September 20 resource-limit receipt](../qualification/devin-live-resource-limit-macos-arm64.json)
does not establish current quota or model availability. These results do not
establish an unrestricted replacement for the native CLIs.

The native `workspace_exec` tool runs bounded Linux commands in an xcb-owned
Lima VM on macOS ARM64. The VM has no host workspace mounts, SSH agent forwarding,
or imported provider credentials. Provider processes keep their existing
confinement. Commands receive a staged workspace and have no network access.

## Setup and admission

Requires Lima 2.2 or later at `/opt/homebrew/bin/limactl`, Python 3, and the source
checkout matching the installed native CLI. The dedicated VM has an 8 GiB sparse
disk, 3 GiB memory, and two CPUs. Setup reserves an eight-GiB host free-space floor
plus its remaining bounded provisioning allocation. It does not reuse other
Lima VMs or import their configuration.

Run from the xcb checkout, using the installed host scheduler where available:

```sh
"$HOME/.bun/bin/host-run" --mode=shared --lane=mac-native \
  --label=xcb-command-setup -- /usr/bin/python3 scripts/setup-command-runner.py \
  --root "$HOME/.local/share/xcb-command" --source "$PWD"
```

The native tool currently uses that exact command root under `$HOME`. The setup
script's `--root` option also supports isolated qualification fixtures; it does
not configure a different root for the native CLI.

Setup installs the trusted supervisor and fixed Linux toolchains: Rust 1.97.1,
Node 24.18.1, and Bun 1.3.14, alongside the recorded Python, Git, C compiler, and
shell. It runs the mandatory synthetic boundary suite before publishing
admission. Admission binds tool and supervisor bytes, sandbox policy, test suite,
Lima/bubblewrap identity, VM boot identity, and complete evidence. Each command
rechecks its admitted environment. Version strings alone do not admit a backend.

After an intentional backend update, stop active commands, install the matching
native CLI, and repeat the setup command with `--refresh`. This preserves the
previous manifest and requires fresh qualification. It does not release an
unsettled job or authorize deleting its records. Restart open xcb terminals and
rerun provider `doctor` after replacing the native CLI.

## Using the tool

After successful setup and admission, ask xcb to run a project check. The
provider can call this closed schema through the workspace broker:

```json
{"argv":["python3","-m","unittest"],"cwd":".","timeoutMs":60000,"network":"none"}
```

`argv` executes directly. Shell syntax requires an explicit `sh -c` argument.
The working directory must be relative to the staged workspace. An unavailable
toolchain or dependency is reported; commands never fall back to the host.
Native macOS, Xcode, Simulator, and arbitrary network commands are unavailable.

| Limit | Command boundary |
| --- | --- |
| Duration | 1 millisecond to 10 minutes; the provider turn has a separate deadline |
| Scratch filesystem | 2 GiB per command |
| Worker memory | 1.5 GiB, with the supervisor outside that worker cgroup |
| Processes | 256 tasks per service |
| Input files | 2 MiB each, 64 MiB aggregate, 8,192 visited entries, 64 path components |
| Published changes | 512 files, 16 MiB aggregate |
| Captured output | 256 KiB combined; capture overflow stops the command |
| Model-facing output | At most 16 KiB each of stdout and stderr, with clipping reported |

Snapshots exclude conventional secret/configuration paths such as `.env`,
provider profiles, `.ssh`, and credential dotfiles; `.env.example`, `.env.sample`,
and `.env.template` remain source inputs. Dependency trees, build products, and
`.xcb-*` staging names are excluded. This is a path policy, not a general secret
scanner. Binary regular files are supported. Symlinks, sockets, FIFOs and other
special entries are never inputs: they are reported as labeled exclusions and
the snapshot continues, so a stray link no longer aborts an otherwise valid
command. Hard-linked or oversized regular files still fail capture closed, and
publication refuses to write through any excluded path.

## Dependencies and Git

Cold dependency installation is unavailable inside an ordinary command. The
separate `scripts/prepare-command-dependencies.py` frontend is now available in
source. Its guest preparation and worker cache attachment passed the current
12-case VM boundary suite, including actual fetch and offline package use.
Installed Claude and Codex coding workflows also passed on macOS ARM64 with the
tested accounts. The frontend refuses a backend without admitted public-cache
support; a successful plan alone does not activate dependency use.

This frontend supports Python 3.9 and newer on macOS; it has been checked with
Apple Python 3.9.6 and Homebrew Python 3.14.6. It observes process exit without
reaping through macOS kqueue, and TOML parsing stays inside the guest.
Run from the matching xcb source checkout. Both paths below must be absolute,
physical paths, and the command root must already belong to xcb.

First inspect the no-download plan. `--dry-run` is also the default. Planning
preserves a private evidence receipt and acknowledges joined guest scratch for
cleanup; it does not publish a cache or change the workspace:

```sh
"$HOME/.bun/bin/host-run" --mode=shared --lane=mac-native \
  --label=xcb-dependency-plan -- /usr/bin/python3 -I \
  scripts/prepare-command-dependencies.py \
  --root "$HOME/.local/share/xcb-command" \
  --workspace /absolute/path/to/project --dry-run
```

After setup admits the matching backend, explicitly prepare the reviewed
public inputs with the same workspace:

```sh
"$HOME/.bun/bin/host-run" --mode=shared --lane=mac-native \
  --label=xcb-dependency-prepare -- /usr/bin/python3 -I \
  scripts/prepare-command-dependencies.py \
  --root "$HOME/.local/share/xcb-command" \
  --workspace /absolute/path/to/project --prepare
```

Preparation records a private durable intent before submitting work. An
interruption, changed input, or incomplete receipt retains that intent and
blocks another preparation. Inspect it using the cache key printed by the plan:

```sh
"$HOME/.bun/bin/host-run" --mode=shared --lane=mac-native \
  --label=xcb-dependency-status -- /usr/bin/python3 -I \
  scripts/prepare-command-dependencies.py \
  --root "$HOME/.local/share/xcb-command" \
  --status --cache-key CACHE_KEY_FROM_PLAN
```

`--status` is read-only. Replace it with `--recover` to stop/join and reconcile
the exact retained guest attempt; recovery never starts another download. Only
a joined terminal receipt clears the host intent, after preserving its result.
Status and recovery take `--cache-key` instead of `--workspace`, so changed
manifests do not prevent selecting the original attempt. Do not delete intent
files or resubmit after an uncertain result. `cleanupPending: true` reports only
that joined temporary scratch still needs cleanup; it does not revoke an exact
`prepared: true` cache receipt. Published immutable caches are retained.

The workspace must contain `Cargo.toml` with a root `Cargo.lock`, `package.json`
with a root `bun.lock`, or both pairs. Matching nested manifests are included;
nested lockfiles are reported but are not covered by the root preparation. A
site with its own lockfile must be prepared separately with that site directory
as `--workspace`, then used as xcb's workspace with
`xcb --cwd /absolute/path/to/project/site`.

Downloads are limited to checksum-bound public crates/npm archives and exact
public GitHub commit sources. Private registries, tokens, SSH credentials, and
ambient package-manager configuration are excluded; dependency installation
scripts are disabled during preparation. The frontend runs no host package
manager or Git command. Ordinary workspace commands remain offline and receive
only the prepared immutable cache.

The cache key binds manifests, lockfiles, toolchain bytes, and preloader bytes;
there is no time-based refresh. Changed inputs require explicit preparation
again, and the worker recomputes the identity before use. A cold or mismatched
cache never enables network access. Internal `plan`/`fetch`/`materialize` phases
are not standalone user entrypoints. The tested cache fixtures do not prove
every dependency-bearing repository can build; prepare its exact inputs and
run its checks inside the admitted workspace.

The Git projection supplies a filtered synthetic repository containing only
selected HEAD and stage-zero index data. The trusted projector runs unprivileged
in a separate networkless namespace; raw repository objects never reach the
command worker. Read-only metadata preserves staged versus unstaged diffs, but
original history, remotes, configuration, hooks, authors, and commit messages
are absent. Supported use is status/diff inspection. Commands cannot change the
host's index, branches, or commits; commit and push workflows are unavailable.

The Git input is bounded to 32 MiB and 4,096 visited entries. Linked worktrees
require a trusted host association. Unsupported or changing Git metadata omits
Git with a diagnostic while allowing ordinary offline commands. The tool reports
`gitInspectionAvailable` and `gitUnavailable`; an unproven projector stop retains
custody and prevents worker execution. Encoded workspace/Git input is limited to
96 MiB. Installed acceptance covers filtered status inspection, not Git writes
or every repository layout.

## Publication and retained state

Only a command that exits successfully, with a complete captured result, no
cancellation or supervisor error, and independently proven join may publish.
Publication checks every changed file against its source snapshot before the
first host write, then checks again immediately before each replacement. xcb
serializes cooperating workspace writers. A rejected revision check preserves
the concurrent source edit and retains the command's staged result.

Each file replacement is atomic; the whole batch is not a transaction. An error
after an earlier file was published can leave partial changes and an uncertain
run that retains account custody. Directory removal, empty-directory changes,
and replacing a file with a directory or vice versa are unsupported. Inspect an
uncertain workspace before deciding how to continue.

After successful publication and durable command/tool settlement, xcb verifies
and removes only that command's owned input snapshot. It preserves historical
records and failed, cancelled, or uncertain inputs. Cleanup failure is reported
without undoing a proven join. The guest can reclaim its acknowledged seed and
unmounted scratch image only after the joined result and changes are durable on
the host; custody and result receipts remain. A cleanup-pending diagnostic is
not permission to delete those records manually.

Retention is an archive, never a deletion. `xcb command prune` reports how many
retained jobs qualify, and `xcb command prune --yes` moves joined, acknowledged
jobs whose newest receipt is older than `--days` (default 30) into
`jobs-archive/` under the same private root. Unjoined jobs, jobs whose guest
scratch cleanup is still pending, and recent jobs are always retained.

## Cancellation, concurrency and recovery

Ctrl-C and SIGTERM request cancellation of a headless run. Installed SIGTERM
cancellation was verified after a guest worker started: guest stop, run
settlement, lease release, and an unchanged workspace were confirmed. The owner
waits for the command's cgroup, descendants, and output streams to join before
releasing account custody. Dropping a provider wait cannot drop that cleanup. A durable
command marker prevents older clients and ordinary host-PID recovery from
releasing an unresolved guest command.

Each account admits one provider turn at a time. Other terminals can view its
session; cancel it in the terminal that owns the turn. Different accounts may
run provider turns concurrently, but the shared command backend admits only one
command at a time. A second command receives a proven unstarted/busy result.
Uncertain jobs block command admission until their exact receipt is reconciled.
Admission checks a `jobs/pending/` marker directory rather than every retained
job, so its cost follows the number of unjoined commands. A command is marked
before guest work can start and the marker is unlinked only after a joined
receipt is durable; durable receipts clear stale markers, and roots created
before markers migrate once on the next admission.

Use `xcb recover` to inspect retained runs. `xcb recover RUN --yes` requires the
original host owner to be gone and independently verifies any pending command
receipt before releasing the account. Recovery never publishes staged edits.
An absent PID, elapsed timer, lost SSH connection, or changed VM boot identity
does not establish completion. Backend refresh is not a recovery shortcut.

The separate application `generate` interface still has zero tools and hooks;
it does not initialize or use the command runner.
