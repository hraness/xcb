<!-- hraness:xcb-landing:start -->
# xcb

Excalibur (`xcb`) brings your coding-agent accounts, model choices, sessions,
and usage into one local terminal workspace. Choose an account, work on your
project, and pick up where you left off without changing interfaces.

The native Rust app is a source preview for supported Claude, Codex, and Devin
runtimes. It includes workspace file tools, an isolated Linux command runner,
customizable panes, and a separate application API. Provider support and
execution boundaries are explicit; it is not a replacement for every feature
of the original provider tools.
<!-- hraness:xcb-landing:end -->

[Project site](https://xcb.sh) · [Getting started](https://xcb.sh/docs/getting-started) ·
[Compare tools](https://xcb.sh/compare) · [Source](https://github.com/hraness/xcb) ·
[Application API](docs/application-api.md) · [Compatibility reference](docs/compatibility.md) · [Contributing](CONTRIBUTING.md)

## Readiness

**xcb is not yet a daily-driver replacement for Codex, Claude Code, and Devin.**
The native broker lists, reads, searches, and writes workspace files, creates
directories, and removes or renames regular files with revision checks. The
source also includes an isolated Linux command runner for tests and builds on
macOS ARM64. The current backend passed its 12-case VM boundary suite, including
filtered Git inspection, public dependency fetching, and offline Cargo/Bun use
from immutable caches. Installed Claude and Codex coding workflows passed on
macOS ARM64: an expected test failure, exact repair, passing test, and filtered
Git status, with joined processes and settled effects. This evidence covers the
tested accounts and admitted builds. Devin's credential-free boundary checks
are separate from authenticated coding acceptance. See the [command runner contract](docs/command-runner.md)
for setup, supported boundaries, and current limits.

| Provider | Native Rust CLI | TypeScript compatibility CLI |
| --- | --- | --- |
| Claude | Installed coding workflow verified on macOS ARM64 with the tested account; Linux remains an execution candidate after sign-in, binary admission, and confinement checks | Execution candidate, subject to its own admission and confinement checks |
| Codex | Native app-server on macOS for exact build **0.155.0-alpha.2.6**; authenticated broker and installed coding workflow acceptance passed on macOS ARM64 with the tested account | Discovery only; managed task execution gated on host qualification |
| Devin | Native ACP candidate on macOS for exact builds **3000.11.1** and **3000.10.31**; both passed credential-free boundary checks; model availability is checked against the connected account's fresh catalog at launch | ACP implementation exists; task execution disabled pending exact-runtime qualification |

A successful `doctor` or a visible model does not prove a working coding session.
The current Devin CLI can be authenticated and can return its model catalog, but
that provider login is separate from XCB's explicit credential import and from
an admitted coding turn. Devin validates the selected model against the
connected account's fresh catalog before each turn. The September 20, 2026
quota result is historical evidence, not a statement of current availability.
Native `doctor` reports `metadata pin only` for unqualified providers. Codex and
Devin candidates require the checked executable digest as well as the version;
other builds and their Linux execution paths remain unavailable. Automated
fixtures check boundaries; they do not establish authentication, service
reliability, or real-model task quality. The TypeScript compatibility CLI's
Codex and Devin task routes remain unqualified and disabled.

## Native xcb

The source build is the current installation path. As of September 19, 2026,
GitHub's latest published release is **AgentMixer v0.3.0**, with an AgentMixer
package archive. No native xcb release or `@hraness/xcb` npm package is published.
Source version 0.4.0 is not a published release. Check the
[release assets](https://github.com/hraness/xcb/releases) before downloading.

### Install from source

Requires Git, the pinned Rust **1.97.1** toolchain, and platform build tools.
Claude's supported execution boundary is macOS Seatbelt or Linux with a working,
admitted `bwrap` configuration. The native Codex and Devin candidates currently
require macOS Seatbelt; unsupported confinement fails closed.

```sh
git clone https://github.com/hraness/xcb.git
cd xcb
rustup toolchain install 1.97.1 --profile minimal
./scripts/install-native.sh
export PATH="$HOME/.local/bin:$PATH"
xcb --version
xcb --help
```

The installer builds with the lockfile and installs `~/.local/bin/xcb`.
`XCB_INSTALL_PREFIX` changes the prefix. Both the old TypeScript CLI and the
native CLI use the name `xcb`; use `command -v xcb` to check which one is active.
The installer also records a private install manifest under the prefix and
keeps the exact installer beside the binary, so later upgrades use the same
verified path.

### Updates and global operation

The native binary is a user-global install when it lives in `~/.local/bin` and
that directory is on `PATH`. XCB follows an OpenCode-style policy: `notify` is
the default, `auto` installs only an exact stable release with its checksum,
and `disable` turns checks off. A macOS LaunchAgent runs the check once a day
when you enable it; it never reads project settings or updates from `main`.

```sh
xcb update check
xcb update enable --policy notify   # check daily and tell you when a release exists
xcb update enable --policy auto     # check daily and install verified releases
xcb update status
xcb upgrade                         # install the latest verified native release
xcb update disable
```

There is currently no published native xcb release, so the updater fails closed
and leaves the source-installed binary alone until the first verified
`xcb-<version>-<platform>-<arch>.tar.gz` release is available. After any
replacement, restart open terminals and rerun `xcb doctor`; provider and
application qualification is bound to the exact installed executable bytes.

Managed supervisors record their exact executable identity. When that binary
is replaced, a current supervisor stops starting new turns, retains custody of
its active workers until they settle, then exits. Queued tasks and tasks waiting
for input remain saved. Wait for that exit, restart the terminal, refresh provider
pins with `xcb doctor`, and reopen the control conversation to continue.
A different running build produces an explicit supervisor-version error.
Legacy supervisors without an identity record need their exact process verified
and stopped after active workers settle; a saved PID or a deleted lock file is
not a safe replacement for that verification.

Managed records can gain fields that older source builds reject. Restart old
clients and supervisors before using updated managed state, and retain that state
during an installation rollback. Replacing the binary does not migrate provider
sessions or establish fresh live acceptance across all three providers.

### First managed conversation

Install an admitted Claude Code binary (major 2, version 2.1.268 or newer).
xcb performs its own account sign-in below; it does
not silently import your existing provider login. Replace `<account-id>` below
with the generated ID printed by `accounts add` or `accounts import-*` (also
listed by `xcb accounts`). Account names come from observed provider identities;
custom labels are not accepted.

```sh
xcb accounts add claude --plan Max
xcb doctor --provider claude
xcb accounts login <account-id>
xcb accounts refresh <account-id>
xcb models
xcb --cwd /absolute/path/to/your/project
```

Plain `xcb` opens a new persistent control conversation. Prompts become durable
managed tasks routed through admitted Codex, Claude, or Devin sessions; closing
the terminal detaches without cancelling them. Open another terminal for an
independent conversation over the same task swarm, use `/tasks` to inspect work,
or `/sessions` to switch control conversations. Ordinary prompts create new work;
when one task in the conversation asks for input, the next reply answers it.
Use `new task: …` to explicitly start separate work. Independent workspaces can
run concurrently; tasks in the same workspace run one at a time. Say
`cancel <task-id>` to request cancellation and inspect `/tasks` for settlement.

The Rust supervisor owns scheduling and deterministic safety decisions. ALGAL
records bounded transition receipts; it does not infer permissions, establish
provider qualification, or replace the supervisor’s execution policy.
`xcb tasks verify <task-id>` replays that task’s local receipt chain and checks
it against the current record; it does not attest provider claims or real-world
outcomes.

Managed routing first filters for qualified, credentialed, idle, quota-usable
accounts. It then ranks bounded model candidates by task fit, fresh remaining
usage, relative quality/cost/latency and Pareto layer, while preserving explicit
provider requests and soft workspace-learned preferences. Official temporary
offers are cached as expiring observations. They do not prove account entitlement
or reduce a route’s estimated cost without that evidence. They never activate an
unqualified provider or survive stale terms. Managed Claude, Codex and Devin workers share
`xcb_swarm_status`, `xcb_message_list` and `xcb_message_send` for durable,
workspace-scoped cross-provider coordination.

A settled authentication failure marks that account as requiring reconnection
and excludes it from new task routes, including after restart. Other eligible
accounts still respect the requested provider. Successful sign-in or an explicit
import with changed credential material clears the block; catalog refresh and
reimporting the same credentials do not. Older failure records have no credential
generation binding, so an upgraded account may need one new bounded attempt to
establish this block.

`--plan` is a display label; it does not verify your subscription. Complete the
browser sign-in when prompted. `accounts refresh` probes supported model and
usage metadata. Unknown or stale usage percentages remain unknown. A proven
Claude account-wide quota exhaustion stays blocked until its reported reset,
even when its percentage has gone stale. The account list shows a retry estimate;
see [quota routing](docs/quota-routing.md) for the scope and credential binding.
For direct sessions, select a model by copying its full observed key from
`xcb models` and running `xcb models default <key>`. Managed tasks choose among
eligible routes; begin a task with `Use Claude`, `Use Codex`, or `Use Devin` to
require that provider.

If discovery finds the wrong binary, use
`xcb doctor --provider claude --executable /absolute/path/to/claude`.
The pin binds executable bytes and version. After upgrading xcb, restart open
xcb terminals and rerun `doctor`. A process started from the old binary cannot
adopt the replacement binary's pin, and older clients refuse new run records
whose credential-custody format they do not understand.
An account, metadata pin, or model listing cannot activate an unqualified adapter.

### Connect Codex on macOS

Use the exact admitted **0.155.0-alpha.2.6** build. Authenticated broker
read/write/read and installed coding-workflow acceptance passed on macOS ARM64
with the tested account; this does not qualify arbitrary provider versions or
the separate application API. xcb supervises the official
CLI's ChatGPT device sign-in in a private profile:

```sh
xcb doctor --provider codex
xcb accounts add codex --plan ChatGPT
xcb accounts login <account-id>
xcb accounts refresh <account-id>
xcb models
```

Follow the device sign-in instructions shown in the terminal. Alternatively,
copy one existing ChatGPT credential into a new xcb account by selecting its
private `auth.json` explicitly:

```sh
xcb accounts import-codex --source /absolute/path/to/auth.json
xcb accounts refresh <account-id>
```

The source file is preserved. Import does not copy provider configuration,
plugins, sessions, or transcripts. API-key credentials are not accepted by this
route. Refreshed ChatGPT credentials are persisted only after the owned provider
process has joined.

### Connect Devin on macOS

Use the exact admitted **3000.11.1** build; **3000.10.31** remains admitted.
Both passed credential-free native boundary checks; authenticated coding
acceptance requires separate evidence for the account, model, and build.
Model availability is checked against the connected account's fresh catalog at
launch. xcb preserves an unknown quota reset as unknown. Sign in through the
provider CLI, then explicitly select its
`credentials.toml` to create a private xcb account:

```sh
devin auth login
xcb doctor --provider devin
xcb accounts import-devin --source /absolute/path/to/credentials.toml
xcb accounts refresh <account-id>
xcb models
```

The source file and provider sessions are preserved. xcb copies only the
credential for the supported provider endpoints. To update just the catalog,
use `xcb models refresh devin --account <account-id>`; Devin discovery requires
an explicitly connected account. Native Devin currently uses fixed ACP model
choices. Adaptive and Fusion catalog representations in the compatibility
package do not establish native support.

For either provider, copy a full matching model key from `xcb models` and set it
with `xcb models default <key>` for direct sessions. Select the account with
`xcb accounts default <account>` for new direct sessions, or pass
`--account <account> --model <key>` to `xcb run`. Rerun `doctor` after a provider
upgrade; a new version is not automatically admitted.

### Isolated tests, builds, and Git

Project commands use an explicitly provisioned Linux VM through `workspace_exec`.
Follow the [command runner setup](docs/command-runner.md#setup-and-admission)
from the same source checkout as the installed native CLI. Commands run offline
against a staged workspace; host dependencies, credentials, and build products
are excluded. Native macOS and Xcode builds are unavailable. The explicit
[public dependency preparation frontend](docs/command-runner.md#dependencies-and-git)
passed the current VM boundary suite, including rejection of a cache after its
manifest changed. Installed Claude and Codex coding workflows passed on macOS
ARM64 with the tested accounts; other repositories and toolchains still need
their own checks.

The Git projection is limited to filtered, read-only HEAD and index data for
status and diffs. Original history, remotes, and hooks are omitted; commit and
push workflows are unavailable. Publication checks file revisions and replaces
each file atomically; it is not a transaction across every changed file. Failed,
cancelled, or uncertain command state is retained. Successfully published and
durably settled commands remove their verified input snapshot.

### Application integration

The [application API](docs/application-api.md) provides bounded, ephemeral
inference with no tools or hooks. It requires evidence for the exact XCB binary,
provider, account and model before accepting application traffic.
[TextButler](https://github.com/hraness/textbutler), an MIT-licensed reference
application, keeps its contact access and messaging approval in its own host.
Sign-in and a successful `doctor` alone do not qualify the application route.

### Everyday commands

```sh
xcb --cwd /absolute/path/to/your/project       # new control conversation
xcb conversations                                # resumable control conversations
xcb chat --resume <conversation-id>
xcb tasks                                        # global managed task swarm
xcb tasks verify <task-id>                       # verify local transition receipts
xcb tasks messages <task-id>                     # durable cross-provider mailbox
xcb offers --refresh                             # refresh official expiring offers
xcb models tiers --task "fix a race"             # inspect Pareto layers
xcb models route --task "fix a race"             # preview the eligible smart route
xcb --cwd /absolute/path/to/your/project run --account <account-id> -p "Explain this repository"
xcb sessions                                     # direct provider sessions
xcb resume                                       # latest direct provider session
xcb resume <session-id>
xcb accounts
xcb config
xcb plugins
xcb panes
xcb doctor
xcb completions zsh > /path/to/completions/_xcb
```

`xcb chat --resume` reopens a control conversation; `xcb resume` opens a saved
direct provider session and its workspace. Neither is a headless continuation
command. `/help` lists terminal commands. Conversations, tasks, provider
sessions, and credentials live in the private native state root
`~/.local/share/xcb`; `--state /absolute/path` or `XCB_STATE` overrides it.
The compatibility CLI uses `~/.xcb` instead. Do not point both implementations
at the same state directory. `xcb --json run` includes the native session ID
in its result so it can be reopened with `xcb resume <session-id>`.

One provider turn has a 30-minute default deadline, including initialization.
The `turn_timeout_ms` setting in the private state root's `config.json` accepts
1,000–3,600,000 milliseconds (one second to 60 minutes); `xcb config` displays the
effective configuration. Older configurations that omit it use the default.
Cancellation remains available before the deadline, and automatic continuation
has its own separate limits.

Control conversations are concurrent and share one durable task supervisor.
Each account still owns at most one active provider turn, and one workspace can
have only one admitted writer even when different accounts or terminals are
available. Independent workspaces and accounts can run concurrently. Managed
cancellation may be requested from the task’s originating conversation or by an
explicit task ID/title elsewhere; direct provider-session cancellation remains
owned by its terminal. Ctrl-C and SIGTERM request bounded cleanup for a headless
run. `xcb run` reports success only for a completed, joined, settled idle result.

Cancellation joins the owned process before releasing custody. If `doctor`
reports an unsettled run, inspect `xcb recover` and the process state; an expired
lease or a quiet terminal is not proof that the provider stopped. Recovery is
an explicit operation, not a reason to delete state or lock files.

### Optional behavior

Auto-continue and Gobstopper context management default on with bounded
continuation and settled-boundary checks. Disable either with
`xcb plugins disable auto-continue` or `xcb plugins disable gobstopper`.
Local usage measurement stays local. aiCharts upload is unavailable; local
exports, external judgment, and executable hooks require separate opt-in.
Panes are presentation data and cannot grant execution authority.

The optional judge uses TypeSafe System One (`jev-latest`) to advise routing,
continuation, and context retention. It sends bounded task/response context to
that service; old tool-result bodies are excluded from compaction advice.
A judge cannot qualify a provider or bypass deterministic safety gates.
Store a key through stdin on macOS or Linux, then explicitly enable it:

```sh
xcb judge token < /secure/path/to/judge-key
xcb judge status
xcb judge enable
xcb judge test
# Later:
xcb judge disable
xcb judge logout
```

The input file is an existing private credential file, not a command-line key.
Keys are vaulted mode-0600 outside the workspace. `XCB_JEV_API_KEY` or
`TYPESAFE_API_KEY` may supply a key via the environment. Custom endpoints require
an explicit environment key; the vaulted key remains bound to System One.

## Migrating from AgentMixer

The native command imports **one Claude credential**, preserving the source:

```sh
xcb accounts import-agentmixer --source /absolute/path/to/.agentmixer --label imported
```

It does not migrate transcripts or sessions. The compatibility source has its
own `migrate` command and identifier changes; see the
[compatibility migration reference](docs/compatibility.md#migrating-from-agentmixer).
Do not run compatibility migration commands against the native state root.

## Standalone package

The retained TypeScript source provides host-owned routing, account custody,
bounded tools, and provider adapters. It is separate from the native Rust app.
For building it locally, library examples, qualification requirements, and its
CLI commands, see the [compatibility reference](docs/compatibility.md) and
[managed Codex contract](MANAGED-CODEX.md). The
[publishing contract](docs/publishing.md) describes future verified artifacts;
it is not evidence of a published package.

## Development

See [Contributing](CONTRIBUTING.md) for setup and the native, compatibility, and
site checks. The credential-free [native Codex boundary fixtures](qualification/codex-native.md)
and [native Devin boundary fixture](qualification/devin-native.md) document
repeatable checks separately from authenticated live acceptance.
Report vulnerabilities through [Security](SECURITY.md). Licensed under [MIT](LICENSE).
