# Isolated workspace commands

The native `workspace_exec` tool lets a task run Linux commands, such as tests
and builds, in an xcb-owned Lima VM on macOS ARM64. The VM has no host folder
mounts, no SSH agent forwarding, and no provider credentials. Each command gets
a staged copy of the project and no network access. Provider processes keep
their own sandbox.

Claude and Codex coding workflows passed on macOS ARM64 with the tested
accounts using this runner: an expected failing test, the repair, a passing
test, and filtered Git status. Other repositories and toolchains need their
own checks.

## Setup

Setup needs Lima 2.2 or later at `/opt/homebrew/bin/limactl`, Python 3, and a
checkout of the xcb source at the same version as the installed `xcb`, because
setup copies the runner's guest files from that checkout. The dedicated VM has
an 8 GiB sparse disk, 3 GiB memory, and two CPUs. Setup keeps at least 8 GiB of
host disk free beyond what it allocates, and it does not reuse other Lima VMs or
their configuration.

Run from the matching checkout:

```sh
git clone --depth 1 --branch v<version> https://github.com/hraness/xcb.git
cd xcb
/usr/bin/python3 scripts/setup-command-runner.py \
  --root "$HOME/.local/share/xcb-command" --source "$PWD"
```

The native tool always uses that command root under `$HOME`. The script's
`--root` option exists for isolated test fixtures; it does not configure a
different root for the native CLI.

Setup installs the runner's supervisor and fixed Linux toolchains: Rust
1.97.1, Node 24.18.1, and Bun 1.3.14, with the recorded Python, Git, C compiler,
and shell. It runs a required test suite before xcb will use the runner, and
records the exact tool and supervisor bytes, sandbox policy, test suite, Lima
and bubblewrap binaries, and VM boot identity. Each command checks that record
again; a version string alone is not enough.

After upgrading xcb, stop active commands, check out the matching source
version, and repeat the setup command with `--refresh`. Refresh keeps the
previous record and reruns the test suite. It does not release a command whose
result is uncertain or allow deleting its records. Restart open xcb terminals
and run `xcb doctor` after replacing the native CLI.

## Using the tool

After setup, ask xcb to run a project check. The provider calls this schema
through xcb's tools:

```json
{"argv":["python3","-m","unittest"],"cwd":".","timeoutMs":60000,"network":"none"}
```

`argv` runs directly; shell syntax needs an explicit `sh -c` argument. The
working directory is relative to the staged project. A missing toolchain or
dependency is reported, and commands never fall back to the host. Native macOS,
Xcode, Simulator, and arbitrary network commands are unavailable.

| Limit | Per command |
| --- | --- |
| Duration | 1 millisecond to 10 minutes; the provider turn has its own deadline |
| Scratch filesystem | 2 GiB |
| Worker memory | 1.5 GiB, with the supervisor outside the worker's cgroup |
| Processes | 256 tasks per service |
| Input files | 2 MiB each, 64 MiB total, 8,192 visited entries, 64 path components |
| Published changes | 512 files, 16 MiB total |
| Captured output | 256 KiB combined; overflowing it stops the command |
| Output shown to the model | At most 16 KiB each of stdout and stderr, with clipping reported |

Snapshots leave out conventional secret and configuration paths such as
`.env`, provider profiles, `.ssh`, and credential dotfiles; `.env.example`,
`.env.sample`, and `.env.template` stay in. Dependency trees, build output, and
`.xcb-*` staging names are left out. This is a path rule, not a secret scanner.
Binary files are supported. Symlinks, sockets, FIFOs, and other special entries
are never copied: they are listed as exclusions and the snapshot continues.
Hard-linked or oversized regular files stop the capture, and publication
refuses to write through any excluded path.

## Dependencies and Git

Ordinary commands can't install dependencies. The
`scripts/prepare-command-dependencies.py` script downloads a project's public
dependencies into a read-only cache that later commands use offline. It needs
Python 3.9 or later on macOS and has been checked with Apple Python 3.9.6 and
Homebrew Python 3.14.6. Run it from the matching xcb checkout; both paths must be
absolute, and the command root must already belong to xcb.

First inspect the plan. `--dry-run` is the default; it downloads nothing and
changes neither the cache nor the project:

```sh
/usr/bin/python3 -I scripts/prepare-command-dependencies.py \
  --root "$HOME/.local/share/xcb-command" \
  --workspace /absolute/path/to/project --dry-run
```

Then prepare the reviewed inputs for the same project:

```sh
/usr/bin/python3 -I scripts/prepare-command-dependencies.py \
  --root "$HOME/.local/share/xcb-command" \
  --workspace /absolute/path/to/project --prepare
```

Preparation records its intent before it starts. An interruption, changed
input, or incomplete result keeps that record and blocks another preparation.
Inspect it with the cache key the plan printed:

```sh
/usr/bin/python3 -I scripts/prepare-command-dependencies.py \
  --root "$HOME/.local/share/xcb-command" \
  --status --cache-key CACHE_KEY_FROM_PLAN
```

`--status` only reads. Replace it with `--recover` to stop and reconcile that
exact attempt; recovery never starts another download. Status and recovery take
`--cache-key` instead of `--workspace`, so changed manifests don't prevent
selecting the original attempt. Don't delete intent files or resubmit after an
uncertain result. `cleanupPending: true` reports only that temporary guest
space still needs cleanup; a cache already marked `prepared: true` stays valid.
Published caches are kept.

The project must have `Cargo.toml` with a root `Cargo.lock`, `package.json`
with a root `bun.lock`, or both pairs. Nested manifests that match are
included; a nested lockfile isn't covered by the root preparation. Prepare a
subproject with its own lockfile separately, with that folder as
`--workspace`, then use it as xcb's project with
`xcb --cwd /absolute/path/to/project/site`.

Downloads are limited to checksummed public crates and npm archives and exact
public GitHub commits. Private registries, tokens, SSH credentials, and your
package-manager configuration are left out, and install scripts don't run. The
script runs no host package manager or Git command. The cache key covers the
manifests, lockfiles, toolchain, and preloader, so changed inputs need a new
preparation; there is no time-based refresh. A cold or mismatched cache never
turns on network access.

Git inside the runner is a filtered, read-only copy of the current commit and
staged index. Status and diffs work, including staged versus unstaged changes.
History, remotes, configuration, hooks, authors, and commit messages are left
out, and commands can't change the host's index, branches, or commits, so
commit and push workflows are unavailable. The Git input is limited to 32 MiB
and 4,096 visited entries. In a linked worktree, Git inspection needs a host
association record that xcb doesn't create for you. Without it, or when Git
metadata is unsupported or changing, Git is left out with a diagnostic and
ordinary commands still run. The project and Git input together are limited to
96 MiB.

## Publication and retained state

Only a command that exits successfully, with complete output, no cancellation
or supervisor error, and confirmed exit may publish its file changes.
Publication checks every changed file against the snapshot before the first
host write, and again just before each replacement. xcb serializes cooperating
writers to the same project. When a check fails, the concurrent edit is kept
and the command's staged result is retained.

Each file replacement is atomic; the whole batch is not a transaction. An error
after an earlier file was published can leave partial changes and an uncertain
run that keeps the account held. Removing directories, empty-directory changes,
and replacing a file with a directory or the reverse are unsupported. Inspect an
uncertain project before deciding how to continue.

After a successful publication, xcb removes that command's own input snapshot
and keeps records and any failed, cancelled, or uncertain inputs. A cleanup
failure is reported without undoing the result. Records of pending cleanup are
not permission to delete them by hand.

`xcb command prune` reports how many old jobs qualify for archiving, and
`xcb command prune --yes` moves finished jobs whose newest record is older than
`--days` (default 30) into `jobs-archive/` under the same folder. Nothing is
deleted, and unfinished, cleanup-pending, and recent jobs always stay.

## Cancellation, concurrency, and recovery

Ctrl-C and SIGTERM cancel a headless run. xcb waits for the command's cgroup,
its descendants, and its output streams to finish before releasing the account.
A marker on each running command keeps older clients and ordinary process
recovery from releasing an unresolved command.

Each account runs one provider turn at a time; other terminals can watch its
session, and the terminal that started the turn cancels it. Different accounts
can run turns at the same time, but the runner runs one command at a time; a
second command gets a busy result without starting. An uncertain command blocks
new commands until it is reconciled.

`xcb recover` lists runs that didn't finish cleanly. `xcb recover <run-id> --yes`
requires the original xcb process to be gone and checks any pending command
before releasing the account. Recovery never publishes staged edits. A missing
process ID, an elapsed timer, a lost SSH connection, or a VM restart doesn't
prove a command finished, and refreshing the runner is not a recovery shortcut.

The application `generate` interface has no tools or hooks and never uses the
command runner.
