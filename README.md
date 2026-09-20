<!-- hraness:xcb-landing:start -->
# xcb

Excalibur (`xcb`) is a metaharness and SDK for working with AI subscriptions.
Its local, terminal-first workspace brings named accounts, local sessions,
token observability, and composable extensions into one Rust and Ratatui
interface. Native adapters for Claude, Codex, and Devin are in development,
with restricted workspace tools and explicit runtime admission.
<!-- hraness:xcb-landing:end -->

[Project site](https://xcb.dev) · [Source](https://github.com/hraness/xcb) ·
[Application API](docs/application-api.md) · [Compatibility reference](docs/compatibility.md) · [Contributing](CONTRIBUTING.md)

## Readiness

**xcb is not yet a daily-driver replacement for Codex, Claude Code, and Devin.**
The native broker can list, read, search, and write workspace files, create
directories, and remove or rename regular files with revision checks. It cannot
run shell commands, tests, builds, Git, or arbitrary provider tools. Run those
operations yourself in a separate terminal and check changes before using them.

| Provider | Native Rust CLI | TypeScript compatibility CLI |
| --- | --- | --- |
| Claude | Execution candidate on macOS/Linux, after sign-in, an admitted binary, and per-run confinement checks | Execution candidate, subject to its own admission and confinement checks |
| Codex | Native app-server execution candidate on macOS for exact build **0.155.0-alpha.2.6**; authenticated live acceptance pending | Discovery only; managed task execution gated on host qualification |
| Devin | Native ACP execution candidate on macOS for exact build **3000.10.31**; authenticated live acceptance pending | ACP implementation exists; task execution disabled pending exact-runtime qualification |

A successful `doctor` or a visible model does not prove a working coding session.
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

### First Claude session

Install an admitted Claude Code binary (major 2, version 2.1.268 or newer).
xcb performs its own account sign-in below; it does
not silently import your existing provider login.

```sh
xcb accounts add claude personal --plan Max
xcb doctor --provider claude
xcb accounts login personal
xcb accounts refresh personal
xcb models
xcb --cwd /absolute/path/to/your/project
```

`--plan` is a display label; it does not verify your subscription. Complete the
browser sign-in when prompted. `accounts refresh` probes supported model and
usage metadata. Unknown or stale quota remains unknown. To select a model, copy
its full observed key from `xcb models` and run `xcb models default <key>`.

If discovery finds the wrong binary, use
`xcb doctor --provider claude --executable /absolute/path/to/claude`.
The pin binds executable bytes and version. After upgrading xcb, restart open
xcb terminals and rerun `doctor`. A process started from the old binary cannot
adopt the replacement binary's pin, and older clients refuse new run records
whose credential-custody format they do not understand.
An account, metadata pin, or model listing cannot activate an unqualified adapter.

### Connect Codex on macOS

Use the exact admitted **0.155.0-alpha.2.6** build. This is a native adapter
candidate; authenticated live acceptance is still pending. xcb supervises the
official CLI's ChatGPT device sign-in in a private profile:

```sh
xcb doctor --provider codex
xcb accounts add codex codex-personal --plan ChatGPT
xcb accounts login codex-personal
xcb accounts refresh codex-personal
xcb models
```

Follow the device sign-in instructions shown in the terminal. Alternatively,
copy one existing ChatGPT credential into a new xcb account by selecting its
private `auth.json` explicitly:

```sh
xcb accounts import-codex --source /absolute/path/to/auth.json --label codex-imported
xcb accounts refresh codex-imported
```

The source file is preserved. Import does not copy provider configuration,
plugins, sessions, or transcripts. API-key credentials are not accepted by this
route. Refreshed ChatGPT credentials are persisted only after the owned provider
process has joined.

### Connect Devin on macOS

Use the exact admitted **3000.10.31** build. This is a native ACP adapter
candidate; authenticated live acceptance is still pending. Sign in through the
provider CLI, then explicitly select its `credentials.toml` to create a private
xcb account:

```sh
devin auth login
xcb doctor --provider devin
xcb accounts import-devin --source /absolute/path/to/credentials.toml --label devin-personal
xcb accounts refresh devin-personal
xcb models
```

The source file and provider sessions are preserved. xcb copies only the
credential for the supported provider endpoints. To update just the catalog,
use `xcb models refresh devin --account devin-personal`; Devin discovery requires
an explicitly connected account. Native Devin currently uses fixed ACP model
choices. Adaptive and Fusion catalog representations in the compatibility
package do not establish native support.

For either provider, copy a full matching model key from `xcb models` and set it
with `xcb models default <key>`. Select the account with
`xcb accounts default <account>` for new interactive sessions, or pass
`--account <account> --model <key>` to `xcb run`. Rerun `doctor` after a provider
upgrade; a new version is not automatically admitted.

### Application integration

The [application API](docs/application-api.md) provides bounded, ephemeral
inference with no tools or hooks. It requires evidence for the exact XCB binary,
provider, account and model before accepting application traffic.
[TextButler](https://github.com/hraness/textbutler), an MIT-licensed reference
application, keeps its contact access and messaging approval in its own host.
Sign-in and a successful `doctor` alone do not qualify the application route.

### Everyday commands

```sh
xcb --cwd /absolute/path/to/your/project run --account personal -p "Explain this repository"
xcb sessions
xcb resume                 # latest native session
xcb resume <session-id>
xcb accounts
xcb config
xcb plugins
xcb panes
xcb doctor
xcb completions zsh > /path/to/completions/_xcb
```

Resume opens the saved session and its workspace. `/help` lists terminal
commands. Sessions and credentials live in the private native state root
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
