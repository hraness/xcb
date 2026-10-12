<!-- hraness:xcb-landing:start -->
# Excalibur (xcb)

> ⚔️ Excalibur (xcb) is an AI subscription router and herder: a control plane
> for Codex and Claude Code subscriptions, built for unattended use. There is
> no app or TUI. You sign into your accounts once in xcb, then ask an agent to
> create whatever topology you want using xcb's primitives. xcb routes tasks
> to accounts based on subscription quotas and health signals, agents
> coordinate through a simple backlog and attention system, and work scales
> up or down with memory, disk, and token pressure.
>
> Tell your agent to set it up: https://xcb.sh
>
> — Ben Guo

Excalibur (xcb) is a control plane for Codex and Claude Code subscriptions,
built for unattended use. Sign in once, and agents route tasks by quota and
health, coordinate through a backlog, and scale with resource pressure.
<!-- hraness:xcb-landing:end -->

**Status:** [Latest release](https://github.com/hraness/xcb/releases/latest)
for macOS ARM64, Linux x86_64 and ARM64, and Windows x86_64; other hosts build
from source. MIT licensed. Commands below match the latest release; a command
that only exists in source appears in the
[release notes](https://github.com/hraness/xcb/releases/latest) when it ships.

[Site](https://xcb.sh) · [Docs](https://xcb.sh/docs) ·
[Getting started](https://xcb.sh/docs/getting-started) ·
[Route contract](docs/route.md) · [TypeScript SDK](docs/sdk.md) ·
[Compare](https://xcb.sh/compare) · [Changelog](CHANGELOG.md)

## Why xcb

- **One pool instead of one login.** Every task goes to an account that can
  take it now: supported build, signed in, idle, not at a known quota limit,
  with a recently seen model. Fresh usage reports rank unused quota that is
  about to reset first. xcb adds no quota of its own and never falls back to
  an API key.
- **Topologies an agent builds.** Projects, tasks, schedules, routes, and hooks
  are commands, so the agent you already use can set up a factory of workers
  and check on it through `xcb tasks` and `xcb attention`. There is no app or
  terminal UI to keep open.
- **Work that scales with pressure.** With resource protection on, xcb samples
  memory, swap, and disk every 30 seconds, stops starting workers when a
  threshold trips, and resumes after three healthy samples. Quota limits a
  provider reports take an account out of the pool until it resets.
- **Your sign-ins, the provider's own tool.** Each task runs Claude Code or
  Codex under your own account in an OS sandbox with xcb's file tools. xcb
  never touches API traffic, and credentials stay on your computer outside your
  projects.

## Install

### Install a verified release

On macOS with Apple silicon or Linux x86_64 or ARM64 (glibc 2.34 or newer), one command
downloads the latest release for your platform, checks its SHA-256 checksum,
and installs `~/.local/bin/xcb`:

```sh
curl -fsSL https://xcb.sh/install.sh | sh
```

On macOS with Apple silicon and Linux x86_64 it also installs
[aicharts](https://aicharts.io/usage) beside xcb, checked against a pinned
SHA-256 digest and, on macOS, its Developer ID signature. On a first install
it turns on local usage history: daily token totals for your agents, kept on
this computer and never uploaded. `aicharts history disable` turns it off.
It also turns on aicharts' daily self-update check, which installs a new
release only after verifying it; `aicharts update disable` turns that off, or
set `XCB_AICHARTS_UPDATE=no` before installing. Set `XCB_USAGE_HISTORY=no` to
leave history off, or `XCB_AICHARTS=no` to skip aicharts.

New release installs update automatically before an interactive `run` or
`doctor` command, at most once a day and only when no other
xcb command or service is using the installation. Run `xcb update disable` to
turn this off, or `xcb update enable --policy notify` for notices only. Existing
saved preferences stay in force. `HRANESS_NO_UPDATE=1`, CI, JSON output, and
noninteractive commands skip automatic updates.

`xcb upgrade` installs the latest release manually. `XCB_VERSION` installs and
pins one exact version, `XCB_INSTALL_PREFIX` replaces `~/.local`, and
`XCB_ADD_PATH=yes` adds the `bin` folder to your shell profile. Source,
package-manager, and older installs without a checksum-bound install record do
not self-update; rerun the release installer to create a supported install.

### Windows

To run Claude Code on Windows, install the Linux build of xcb inside [WSL2](https://learn.microsoft.com/windows/wsl/install) with
the command above. Claude is the supported provider on Linux; Codex
requires macOS. Releases also carry a native
Windows x86_64 build for local task inspection, `xcb doctor`, accounts, and
`xcb route`, which refuses provider work with those WSL2 steps. Install it from PowerShell:

```powershell
irm https://xcb.sh/install.ps1 | iex
```

It checks the zip's SHA-256 checksum and installs
`%LOCALAPPDATA%\Programs\xcb\bin\xcb.exe`; state lives in
`%LOCALAPPDATA%\xcb`. The binary is not code-signed yet, so SmartScreen may
ask before its first run.

### Build from source

On other hosts, build with Git, Rust 1.97.1, and the platform's build tools:

```sh
git clone https://github.com/hraness/xcb.git && cd xcb
rustup toolchain install 1.97.1 --profile minimal
./scripts/install-native.sh
```

[Upgrade and uninstall](https://xcb.sh/docs/upgrade-and-uninstall) covers
updates and removal.

## Your first task

Install Claude Code 2.1.268 or later, then connect an account and run a task:

```sh
xcb setup claude
xcb run -p "Explain this repository"
```

`xcb setup` lets you choose an existing account or add another, checks the
Claude Code build, opens browser sign-in, and loads the account's models. New
Claude accounts keep a model-only token in xcb's state folder. Accounts connected
for shared browser access use a dedicated xcb Keychain entry on macOS. Both are
separate from your usual Claude Code login. `xcb setup codex`
works the same way; see [accounts and models](https://xcb.sh/docs/providers)
for sign-in and import options.

`xcb run` picks an account and model and prints the result. `xcb --json route`
runs exactly one turn and returns a JSON result; its caller owns any retry.

## How work moves

Sign in once. Then an agent, or you, creates projects, tasks, schedules,
routes, and hooks. xcb runs each task on one account and keeps the backlog
and the attention queue so agents can coordinate without you watching.

```text
 your agent ──creates──▶  xcb primitives   ──one task,──▶  your accounts
 (Claude Code,            projects · tasks    one account    Claude and Codex
  Codex, or an app)       schedules · routes                 subscriptions
            ◀──questions,  hooks              ◀──quota, limits──
               results    backlog · attention
```

For durable managed work, submit an explicitly scoped backlog item:

```sh
xcb backlog add /absolute/path/to/project "Fix the failing test" --ready
xcb schedules add /absolute/path/to/project "Review previous work" --every 3600
xcb tasks                                # managed tasks across projects
xcb attention                            # questions and approvals waiting on you
```

Backlog tasks run in xcb's background supervisor and outlive the terminal that
created them; `xcb service install` starts the supervisor at login on macOS and
Linux. Inspect a task with `xcb tasks show <task-id> --json`.

- `xcb tasks cancel <task-id> --revision <revision>` requests cancellation.
- `xcb steer <task-id> <guidance>` adds guidance for the task's next turn.
- `xcb attention` shows questions; `xcb backlog reply` answers one.
- `xcb conversations --new --json` creates a project view without opening a UI.

[Project controls](docs/project-agents.md) cover grants, programs, and
schedules; the [headless command guide](docs/terminal.md) covers local
operations.

## Use available quota before it resets

1. **Filter:** keep the accounts that can take the task now: supported provider build, signed in, enabled, idle, not at a known usage limit, with a recently seen model.
2. **Rank:** order those models by relative quality, cost, and speed for the kind of task. Fresh Claude and Codex usage reports favor unused quota approaching a reset within the task's quality requirements. Long prompts get the highest-quality model available.
3. **Hold:** lock the chosen account so no other task can use it, and run the provider in an OS sandbox with xcb's file tools for one project folder.
4. **Record:** when the provider process exits, record how the run ended. If xcb can't confirm that, it keeps the account held and doesn't retry.

[How routing works](https://xcb.sh/docs/how-routing-works) covers each step.

Tasks that require an existing signed-in browser or native desktop control
stay with Codex and prefer Astra, including after a retry or handoff. Use
`xcb run --signed-in-browser` or `xcb run --desktop` to state that requirement.
Claude can hand these tasks to Codex after their current run ends
safely. `xcb tools setup-computer` connects the installed desktop computer-use
plugin on macOS. `xcb tools setup-browser` shares Claude's Chrome extension
across providers and opens full Claude sign-in when needed. This sign-in uses
a dedicated xcb Keychain entry on macOS. See
[browser and shared tools](docs/tools.md).

## Run unattended

`xcb resources enable` turns on resource protection. The supervisor then
samples memory pressure, swap, and available disk every 30 seconds and defers
new workers when the host runs short, using these defaults:

| Condition | Behavior |
| --- | --- |
| Disk available space at or below 24 GiB | Include a resource advisory at the next worker turn |
| A known volume reaches 8 GiB | Defer new workers using it |
| A paused volume recovers above 12 GiB | Require three healthy samples before resuming |
| Memory warning or critical pressure lasts at least 60 seconds | Defer new workers until three normal samples arrive |
| Required measurements fail or are older than 120 seconds | Defer new workers until measurements recover |

An advisory is host telemetry only; xcb never kills a process or deletes a
cache in response. Thresholds live in the `resources` section of
`config.json`. Schedules can stop waiting on an unanswered task after an
interval you choose with `--settle-unanswered-after <seconds>`. The
[unattended operation guide](docs/unattended.md) covers the login service,
recovery of uncertain work, and what still needs a person.

## Continue a Claude or Codex conversation

Bring conversation context into xcb so your next task can use its account and
model selection. Discovery and import find work active in the last 24 hours by
default:

```sh
xcb sessions discover
xcb sessions import --recent
xcb conversations                           # saved conversations, including imports
xcb history <conversation-id>
xcb backlog add <conversation-id> "Continue the work" --ready
```

Submit a backlog task in the imported conversation to start work. Import copies user and
assistant text, preserves the original files, and does not take over the
provider process. For a conversation started in your home folder, select one
result with `xcb sessions import <candidate-id> --workspace /path/to/project`.
[Session import](docs/session-import.md) covers provider filters and limits.

## Build on it

**From an agent or script,** `xcb --json route` reads one JSON task on stdin,
picks an account and model that can take it, runs one turn, and prints one
JSON result:

```sh
echo '{"version":1,"workspace":"/absolute/path/to/project","task":"Fix the failing parser test"}' \
  | xcb --json route
```

```json
{"version":1,"status":"completed","requestId":"route_…","session":"s_…",
 "route":{"provider":"claude","account":"a_…","model":"claude/sonnet/low","label":"Sonnet · low","reason":"…"},
 "state":"idle","outcome":{"terminal":"completed","joined":true,"effects":"settled","pending_attention":false,"failure":null},
 "text":"…"}
```

Add `"dryRun": true` to see the chosen route without running anything, or pin
`provider`, `account`, or `model`. A failure exits 1 with a `code` such as
`unavailable`, `busy`, or `needs_input`. The [route contract](docs/route.md)
lists every field.

**From your own app,** the TypeScript SDK's `createSubscriptionRouter` runs a
task on the account and model your app names, and holds that account until
the provider process exits; it does not choose them for you. Install it with
`npm install @hraness/xcb`; the [SDK quickstart](docs/sdk.md) has a complete
example.

## Providers

| Provider | Supported builds | Status |
| --- | --- | --- |
| Claude | Claude Code 2.1.268 or later within version 2 | Coding workflow passed on macOS ARM64 with the tested account. On Linux, Claude runs after you run xcb's sandbox checks on that machine. |
| Codex | Codex CLI 0.159.0, 0.158.0, 0.157.1, or 0.156.1 on macOS ARM64 | Coding workflow passed on macOS ARM64 with the tested account and Codex CLI 0.158.0. |

xcb checks each provider executable's version, and for Codex its
exact SHA-256, before it runs anything. `xcb doctor` shows what it found.

## Everyday commands

```sh
xcb --help                             # discover local commands
xcb conversations --new --json          # a project view for this directory
xcb run -p "Explain this repository"   # one task here; prints the answer
xcb tasks                              # managed tasks across projects
xcb attention                          # questions and approvals waiting on you
xcb accounts                           # accounts, usage, and which need you
xcb usage                              # your token use by day, agent and model
xcb doctor                             # provider builds and unfinished runs
xcb upgrade                            # install the latest verified release
xcb help advanced                      # project agents and extensions
```

Accounts, credentials, and task history live in `~/.local/share/xcb`, outside
your projects (`--state` or `XCB_STATE` moves it). The
[CLI and configuration reference](https://xcb.sh/docs/reference) lists every
command, setting, and exit code.

## Optional judges

The judge is off by default. Set `CLOUDFLARE_ACCOUNT_ID` (32 hexadecimal
characters) and `CLOUDFLARE_API_TOKEN` in your trusted host environment, then
run `xcb judge enable`. `CLOUDFLARE_AUTH_TOKEN` also works. Clef requests use
your Cloudflare Workers AI account and have separate provider charges.

`xcb judge clef --model clef-flash` selects the alternative model; the default
is `clef`. `xcb judge status`, `xcb judge test`, and `xcb doctor` do not send
inference requests. `xcb judge disable` stops judge use. The judge advises
routing and can veto safe continuation or context elision; it cannot approve
a task, grant tools, or override deterministic safety checks.

The native CLI also supports xAI, Vercel AI Gateway, and custom compatible
endpoints. `xcb judge select xai` selects Grok 4.7; credentials stay separate
from coding accounts. See [API judge setup](docs/chat-judge.md) for private
key storage and an explicit synthetic connection test.

[Project controls](docs/project-agents.md) cover scoped unattended work.
The [operations guide](docs/unattended-operations.md) explains retry, account
health, completion checks, and recovery limits.

SDK callers can supply embedded PNG, JPEG, or WebP evidence explicitly: up to
four images, 4 MiB and 16 megapixels each, 8 MiB total, and a 13 MiB request.
xcb never captures screenshots automatically. See the
[judge API and legacy configuration](docs/compatibility.md#judged-routing-continuation-and-compaction-optional).

## See your token use

`xcb usage` shows your token use across coding agents by day, agent, provider,
and model. The numbers come from [aicharts](https://aicharts.io), which keeps a
daily record on your computer and uploads nothing. The installer above adds
aicharts; after another install method, [get it](https://aicharts.io/usage).
`xcb usage enable` has aicharts collect four times a day, and
`xcb usage report --csv` exports the rows. `xcb usage connect` gives Claude and
Codex tasks aicharts' read-only usage tools, so an agent you route can answer
questions about your token use or chart it; run it again after updating
aicharts. `xcb doctor` shows whether the record is collecting and the tools are
connected. Quota left on each subscription stays in `xcb accounts`.

## Limits

- **Tools:** providers use xcb's workspace tools and registered host MCP servers. Native commands require separate provider qualification and workspace grants; unrelated provider plugins remain unavailable. See [provider permissions](docs/provider-permissions.md) and [browser and shared tools](docs/tools.md).
- **Tests and builds:** the [offline command runner](docs/command-runner.md) uses a Linux VM on macOS ARM64 with read-only Git. Separately granted native execution can run host builds and delivery commands; it never substitutes for a failed offline replay.
- **Concurrency:** each account runs one provider turn at a time by default; `max_runs_per_account` in `config.json` (1–32) raises how many tasks may share an account, while sign-in and account checks still take the account alone. Tasks in the same project folder take turns.
- **One machine:** xcb runs the accounts and tasks on the computer it is installed on. There are no hosted remote commands.
- **Managed harness:** the self-tuning harness is in development; the current build does not run self-modifying routing policies.

## Check it

Every release is built and checked in a public GitHub Actions run linked from
the [install page](https://xcb.sh/install), and the installer verifies the
archive's SHA-256 checksum before it installs anything. On your machine,
`xcb doctor` reports the provider builds it found and any unfinished runs, and
`xcb tasks verify <task-id>` replays a task's local record offline.

## Compared with

- **Claude Code or Codex alone:** enough when one subscription covers your work, and you keep all of the tool's built-in tools, MCP servers, and plugins. xcb supplies workspace tools, its offline command runner on macOS, and registered host tool servers across providers.
- **Account switchers such as [claude-swap](https://github.com/realiti4/claude-swap):** change which login Claude Code uses. xcb picks an account for each task across Claude and Codex, and sandboxes each run.
- **[Claude Code Router](https://xcb.sh/compare/claude-code-router) and [OpenRouter](https://xcb.sh/compare/openrouter):** send each API request to a provider or model you choose, usually paid per token. xcb never touches API traffic; it routes whole tasks to subscriptions you already pay for.
- **[Conductor](https://xcb.sh/compare/conductor) and Claude Squad:** give each agent a Git worktree and a merge flow. xcb uses project grants and worker commands for delivery, with read-only PR observation for pending checks.

[All comparisons](https://xcb.sh/compare)

## More

The name xcb is short for Excalibur. xcb was formerly AgentMixer.
The [compatibility reference](docs/compatibility.md) covers the TypeScript
package and its `xcb-compat` CLI. Supported Unix Bun/npm global copies update
before interactive work; `xcb-compat update disable` turns that off. SDK imports
never update. [Contributing](CONTRIBUTING.md) ·
[Security](SECURITY.md) · [MIT license](LICENSE)
