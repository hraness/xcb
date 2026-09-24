# Native Claude fixture

An additional experimental kernel probe compiles a disposable C helper and
checks a default-deny macOS Seatbelt profile using only synthetic files and a
local endpoint:

```sh
hra-host-run --mode=shared --lane=mac-native --label=xcb-kernel-boundary-probe -- bun qualification/macos-sandbox.ts
hra-host-run --mode=shared --lane=mac-native --label=xcb-native-claude-os-scope -- bun qualification/claude-native.ts --os-sandbox
```

The second command applies that same experimental profile to the actual native
Claude fixture below. Neither command enables a production adapter or issues a
production qualification. The profile allows only the pinned executable,
specified system libraries and loader paths, private scratch directories, standard
I/O descriptors, and one local TCP port. The helper checks foreign file, directory
and FIFO reads, foreign writes, process creation, other executables and other
network ports. Re-execution of the same pinned binary is permitted by the profile;
fork and other executable paths remain denied. This is an explicit diagnostic
for the current Mac, not a portable sandbox guarantee. Production use also needs
a reviewed provider relay and exact distribution/account admission.

Current result on the tested Darwin ARM64 host (re-verified 2026-09-17): the
converged kernel helper passes its nine checks, the bounded
`macos-native-bootstrap.ts` diagnostic runs the pinned executable's `--version`
and completes the SDK `--initialize` control handshake under the experimental
profile, and the full `claude-native.ts` fixture passes all seven scenarios under
`--os-sandbox` — see `2026-09-17-darwin-arm64.json`. The experimental profile
learned three grants the real binary needs that the earlier revision lacked:
`file-read-metadata` on the `/tmp`/`/private/tmp` literals (Claude's mkdirp
stat-walk for its per-user `claude-<uid>` dir — Seatbelt evaluates metadata ops
on the unresolved path while data ops report canonicalized paths), ancestor
metadata for each scratch root, and the reviewed resolver surface
(`/etc`/`timezone`/`usr/share` reads, opendirectoryd + DNSConfiguration lookups,
mDNSResponder/syslog sockets) behind a `runtimeSurface` flag. Egress stays
pinned to the synthetic loopback port; `git`/`sh` exec and real-home reads
remain denied and are part of what the fixture proves. All evidence stays
synthetic — zero paid model requests, no real credentials — and remains no
native production qualification.

Run the explicit fixture on macOS ARM64 through the installed host scheduler:

```sh
hra-host-run --mode=shared --lane=mac-native --label=xcb-native-claude-scope -- bun qualification/claude-native.ts
```

It runs the actual pinned native Claude Code binary through the real Agent SDK,
using a fresh synthetic home, fake API key, local synthetic Anthropic Messages
endpoint and disposable synthetic contact folders. It does not load a real account
or make paid model calls. It shares the production restricted option builder;
custom endpoints are confined to this fixture.

The scenarios verify zero-tool classification; denied built-in command, file,
agent, skill, network and tool-discovery calls; allowed broker conditional edits
and staged replies; and rejected absolute/traversal/symlink/hardlink reads and
stale or escaping writes. Explicit Skill `doctor` / `checkup` calls must return
errors, and wrapped `/doctor`, `/checkup` and bang commands must reach the synthetic
API as the exact literal task text. Every request must advertise the exact broker
manifest. Poisoned settings, hook, MCP and instruction fixtures
exercise inheritance. A sibling canary and command marker detect effects, and
actual native tool results establish denial independently of absent effects.

A successful JSON receipt records the binary/SDK identity, platform and exact
scenarios. It is **not** a production qualification receipt. Its scope is the
model-visible tool boundary in this synthetic environment. It does not prove an
OS sandbox against a compromised provider executable, exclusion of every managed
system policy, prohibition of process-group escape by trusted helpers, or live
provider/model/account behavior. Do not convert it into `RuntimeQualification`
without reviewing the deployment's remaining requirements and evidence.

The fixture uses the vendored `contact-workspace.ts` synthetic contact
workspace, a copy of the consumer's confined file boundary, because consumers
supply the filesystem enforcement. Xcb itself continues
to depend only on its generic file broker port.

The pinned native runtime retains `doctor` in its discovery catalog with only an
empty skills allowlist. The shared production builder also sets the documented
`skillOverrides` for `doctor` and `checkup` to `off`; the native fixture proves those
restrictive settings and keeps the empty catalog assertion intact. The
[SDK skills documentation](https://code.claude.com/docs/en/agent-sdk/skills) explains
why discovery metadata and execution authority are different.

## Live Claude subscription smoke

`2026-09-24-live-claude-subscription.json` records the runtime boundary holding
on Claude Code **2.1.282** (and 2.1.281 earlier the same session) after 2.1.281
began listing its builtin `agents-md` plugin at session start. Without the
launch setting that disables that plugin, the per-run boundary assertion failed
closed on every native Claude route; with it, an automatically routed coding
turn from a disposable scratch repository completed with settled effects, the
provider joined before the completion was persisted, and the requested file was
written. The receipt names the runtime, the boundary assertions, the prior
failure, and what it does not establish.

`2026-09-22-live-claude-subscription.json` supplements the 2026-09-17 receipt
below rather than replacing it. It answers one question the earlier one cannot:
whether the admitted-version floor holds live on a runtime newer than the one
that receipt attests.

It records one user-operated CLI turn on `main` at `ccefc0a`, from a release
binary built from that commit with no source modified for the run. The installed
runtime was **2.1.278**, admitted by the `MIN_VERSION` floor and major ceiling
rather than by an exact pin. `claude/sonnet/low` returned the required text from
a disposable empty workspace that held no entries afterwards; the reported effect
state was `none`, and the provider process joined before the completion was
persisted. The per-run boundary assertion — including the empty skills and
plugins inventories — passed on that runtime.

One difference from the 2026-09-17 receipt is recorded rather than inherited:
run artifacts are **not** removed after the provider joins. Each launch retains
a private per-run copy of the provider executable (0500) and its generated
Seatbelt policy (0600) under the state directory (0700). Nine launch directories
were present on this host, each holding its own copy of the 217 MB executable.

What it does not establish is on the receipt itself: it qualifies this runtime,
not every future release inside major 2. The per-run boundary assertion remains
the enforcement; the floor only decides which binaries `doctor` will offer.

`2026-09-17-live-claude-subscription.json` records one user-operated CLI turn
against the real Claude subscription service. The exact admitted 2.1.268 runtime
returned the required text from `claude-sonnet-4-5`; the completed transcript was
persisted only after the bounded provider process joined, and the per-run runtime
snapshot and Seatbelt policy were then removed. The receipt binds the public main
tree, runtime, profile and local admission digests. It records no token, account
identity, authorization URL, local path or session identifier.

This closes evidence for one authenticated live-provider turn on the tested
Darwin host. It does not measure subscription usage, qualify other models or
hosts, activate a production deployment, or turn the local seven-day CLI
admission into a general production qualification. The synthetic confined fixture
above remains the independent evidence for effective tool inventory and escape
denials; the live smoke does not replace it.

## Native DNS and TLS regression

Run `qualification/native-network.py --output /tmp/xcb-native-network.json` with
`/usr/bin/python3` through the host scheduler's `mac-native` lane. This
credential-free macOS check extracts the current Codex and Devin metadata-session
Seatbelt templates, preloads Python's codecs and public CA context, and tests DNS,
TCP and TLS against fixed public HTTPS endpoints. Each production policy must
succeed, while removing only the `/var` symlink metadata grant must reproduce the
DNS failure. The receipt binds source and policy hashes and proves every child
process group joined; it contains no credentials, response bodies or local paths.
This checks the resolver regression, not provider authentication, model behavior,
or complete confinement. The native provider fixtures and live acceptance remain
separate requirements. The checked-in [network receipt](native-network-macos-arm64.json)
records the tested host and policy source; rerun it after relevant policy changes.

## Current native Claude kernel boundary

Run `/usr/bin/python3 qualification/claude-kernel.py --output /tmp/xcb-claude-kernel.json`
through the host scheduler's `mac-native` lane. This synthetic check extracts the
current Rust `sandbox::seatbelt` policy, allows scratch operations, and requires
foreign consumer, peer-account, ambient-config and shared global temp (including
its `/tmp` alias) reads, writes, symlink access, hard-link creation and renames to
fail. It separately verifies permitted fork and
denied `/bin/sh` execution, checks complete protected-file and directory identity
plus canary bytes, and joins every child process group. The
[checked-in receipt](claude-kernel-macos-arm64.json) binds the current policy and
installed Claude executable hash and package version. Python is initialized before
confinement, and Claude is not executed: this is kernel policy evidence, separate
from native tool inventory, authentication and live-provider qualification.

## Application qualification prerequisites

`application-prerequisites.py` collects actual native validation output and
prepares the private input for `xcb --json qualify-application`. It does not
perform authenticated inference or activate application access. Python 3.9 or
newer, Git, Cargo, the final release xcb executable, and that executable's current
doctor pins and account/model catalog are required. The `--state` directory is
existing private xcb state; the script inspects it through the read-only CLI and
never reads credential files itself.

Freeze the source and build `cargo build --release --locked -p xcb-cli`. Use the
same final bytes for doctor, collection, qualification, and the calling
application. Run the following through the installed host scheduler, with one
integration owner. Replace every example path and the account/model selection:

```sh
/absolute/path/to/hra-host-run --mode=exclusive --lane=mac-native --label=xcb-application-prerequisites -- \
  /usr/bin/python3 /absolute/xcb/qualification/application-prerequisites.py collect \
  --xcb /absolute/xcb/target/release/xcb --state /absolute/private/xcb-state \
  --source /absolute/xcb --provider claude --account a_selected --model claude/sonnet/low \
  --provider-executable /absolute/path/to/native/claude \
  --output /absolute/private/new-application-evidence
```

Collection runs the required Cargo formatting check, workspace tests and Clippy,
then builds the release CLI and requires its digest to equal `--xcb`. Cargo's
compiler-artifact message identifies the actual executable, including configured
output directories and targets. The actual
workspace test log supplies the mandatory application unit and contract cases,
so those tests are not run twice. Native source files, embedded fixtures, Cargo
manifests/lockfile, local Cargo configuration, toolchain and compiler environment
are checked before and after collection. xcb's inspection command supplies the
exact compiled policy/configuration, executable and provider identities.

For Claude, collection runs the current credential-free kernel probe and checks
its exact policy/harness binding, required syscall cases, protected canaries and
process joins. It does not execute Claude. Codex and Devin instead require an
explicit `--provider-boundary /absolute/current-native-boundary.json` from their
separate reviewed fixtures; the script checks the supported receipt schema,
current binary/source bindings and successful observations. Devin's helper must
match the final xcb binary. Run those fixtures again when their bindings change.
Keep their original observation times; a boundary receipt is never refreshed by
copying it into a new bundle.

Only a complete successful collection publishes `prerequisites.json` and its
content-addressed artifacts. Directories are mode0700 and files mode0600. Failed
collections remain for diagnosis and cannot be used as qualification evidence.
The script has no option for supplying a success flag, exit status, custom
command, prompt, or replacement collection timestamp. Its `capture.json` and
per-command execution records bind actual commands, exit codes and raw bytes.
These are trusted local execution records, not signatures against an actor who
already controls the host account.

After collection, use the same final executable for the separate fixed live
challenge, also through the scheduler:

```sh
/absolute/path/to/hra-host-run --mode=shared --lane=mac-native --label=xcb-application-live-qualification -- \
  /absolute/xcb/target/release/xcb --json --state /absolute/private/xcb-state \
  qualify-application --account a_selected --model claude/sonnet/low \
  --evidence /absolute/private/new-application-evidence
```

Qualification is limited to that exact account/model and replaces its previous
coverage. The runtime publishes a receipt only after the common application
executor completes the exact synthetic challenge, joins its work and settles
account/auth custody. Application requests repeat admission under the account
lease. Sign-in replacement, executable changes and policy/configuration changes
invalidate qualification immediately.

The receipt expires at the earliest prerequisite observation plus24hours.
Capabilities reads do not renew it. To renew, rerun the same collection and live
qualification with a new output directory and fresh boundary observations.
`bundle --capture /absolute/private/existing-capture` accepts the same selection,
source/runtime arguments and a new `--output`; it rechecks and recopies an existing
successful capture while preserving its original deadline. It cannot renew old
evidence or turn a failed command into a success.

Run `python3 qualification/application-prerequisites.py --self-test` through the
host scheduler for the hermetic parser, custody and bundle regressions. These
checks use only synthetic files and a bounded Python signal-mask child; they do
not run Cargo, xcb or providers.
