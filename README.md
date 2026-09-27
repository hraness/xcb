<!-- hraness:xcb-landing:start -->
# xcb

xcb routes coding tasks across the Claude, Codex, and Devin subscriptions you
already pay for. Each task runs on an account that is signed in, idle, and not
at a known usage limit, on a model that fits the work. Type work into xcb's
terminal thread, where tasks keep running after you close the terminal, or
hand it one task at a time from another agent or your own code.
<!-- hraness:xcb-landing:end -->

**Status:** [Latest release](https://github.com/hraness/xcb/releases/latest)
for macOS ARM64 and Linux x86_64; other hosts build from source. MIT licensed.

[Site](https://xcb.sh) · [Docs](https://xcb.sh/docs) ·
[Getting started](https://xcb.sh/docs/getting-started) ·
[Route contract](docs/route.md) · [TypeScript SDK](docs/sdk.md) ·
[Compare](https://xcb.sh/compare) · [Changelog](CHANGELOG.md)

## Install

### Install a verified release

On a supported platform, the installer downloads one release archive, checks
its SHA-256 checksum, and installs `~/.local/bin/xcb`. Replace `<version>`
with the number on the [latest release](https://github.com/hraness/xcb/releases/latest),
without the leading `v`:

```sh
curl -fsSLO https://raw.githubusercontent.com/hraness/xcb/v<version>/scripts/install-native.sh
XCB_VERSION=<version> sh install-native.sh
export PATH="$HOME/.local/bin:$PATH"
```

The installer refuses a checksum mismatch or an archive that holds anything
but the `xcb` binary, keeps the binary it replaces as `xcb.previous.<sha256>`,
and records itself so `xcb upgrade` installs later releases the same way.
`XCB_INSTALL_PREFIX` replaces `~/.local`; `XCB_ADD_PATH=yes` adds the `bin`
folder to your shell profile.

### Build from source

On other hosts, build with Git, Rust 1.97.1, and the platform's build tools:

```sh
git clone https://github.com/hraness/xcb.git && cd xcb
rustup toolchain install 1.97.1 --profile minimal
./scripts/install-native.sh
```

[Upgrade and uninstall](https://xcb.sh/docs/upgrade-and-uninstall) covers
updates and removal.

## Use it as your coding agent

Install Claude Code 2.1.268 or later, then connect an account and open your
thread:

```sh
xcb setup claude
xcb
```

`xcb setup` adds an account, checks the Claude Code build, opens the browser
sign-in, and loads the account's models. xcb keeps that sign-in in its own
state folder, apart from your usual Claude Code login. `xcb setup codex`
works the same way; Devin connects by importing the Devin CLI's sign-in
([accounts and models](https://xcb.sh/docs/providers)).

Plain `xcb` opens your thread, one conversation for all your projects. Type a
task such as “fix the failing test in ~/src/app”. xcb picks the project folder
and says why (“Started Fix the failing test in `app` · named `app` ·
/workspace to move”), picks an account and model, and runs the task there. If
a turn stops at a usage limit, xcb continues the task on another account or
model that can take it. Closing the terminal detaches without cancelling
anything; the next `xcb` shows the results.

- `/tasks` lists running and finished work; `/cancel <task-id>` stops a task.
- `/steer <task-id> <guidance>` adds guidance for a task's next turn.
- Start a prompt with `Use Claude`, `Use Codex`, or `Use Devin` to choose the
  provider. `/help` lists every command, and the
  [terminal guide](docs/terminal.md) covers keys and search.

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
the provider process exits; it does not choose them for you. It ships as the
`hraness-xcb-<version>.tgz` release archive, not on npm. See the
[SDK quickstart](docs/sdk.md).

## Providers

| Provider | Supported builds | Status |
| --- | --- | --- |
| Claude | Claude Code 2.1.268 or later within version 2 | Coding workflow passed on macOS ARM64 with the tested account. On Linux, Claude runs after you run xcb's sandbox checks on that machine. |
| Codex | Codex CLI 0.156.1 on macOS ARM64 | Passes xcb's sandbox and tool checks. The recorded signed-in coding run used the previous supported build. |
| Devin | Devin CLI 3000.11.3, 3000.11.1, or 3000.10.31 on macOS ARM64 | The supported builds pass xcb's sandbox checks, but a coding session on a signed-in Devin account hasn't been confirmed. |

xcb checks each provider executable's version, and for Codex and Devin its
exact SHA-256, before it runs anything. `xcb doctor` shows what it found.

## How it works

1. **Filter:** keep the accounts that can take the task now: supported provider build, signed in, enabled, idle, not at a known usage limit, with a recently seen model.
2. **Rank:** order those models by relative quality, cost, and speed for the kind of task. Long prompts get the highest-quality model available.
3. **Hold:** lock the chosen account so no other task can use it, and run the provider in an OS sandbox with xcb's file tools for one project folder.
4. **Record:** when the provider process exits, record how the run ended. If xcb can't confirm that, it keeps the account held and doesn't retry.

[How routing works](https://xcb.sh/docs/how-routing-works) covers each step.

## Everyday commands

```sh
xcb                                    # your thread, from any directory
xcb chat --new                         # a project view for this directory
xcb run -p "Explain this repository"   # one task here; prints the answer
xcb tasks                              # managed tasks across projects
xcb attention                          # questions and approvals waiting on you
xcb accounts                           # accounts, usage, and which need you
xcb doctor                             # provider builds and unfinished runs
xcb upgrade                            # install the latest verified release
xcb help advanced                      # remote devices, project agents, extensions
```

Accounts, credentials, and task history live in `~/.local/share/xcb`, outside
your projects (`--state` or `XCB_STATE` moves it). The
[CLI and configuration reference](https://xcb.sh/docs/reference) lists every
command, setting, and exit code.

## Limits

- **Tools:** providers work through xcb's file tools, without their own shells or plugins, so a task can do less than in the provider's own CLI.
- **Tests and builds:** the [command runner](docs/command-runner.md) is an offline Linux VM on macOS ARM64; Git is read-only there, and native macOS builds can't run.
- **Concurrency:** each account runs one provider turn at a time, and tasks in the same project folder take turns.
- **Remote devices:** `xcb link` needs a relay deployed from this repository's `convex/` folder ([remote operations](docs/remote-operations.md)).
- **Managed harness:** the self-tuning harness is in development; the current build does not run self-modifying routing policies ([design](docs/managed-harness.md)).

## More

xcb was formerly AgentMixer: `xcb accounts import-agentmixer --source <path>`
copies one Claude credential ([migrating](docs/compatibility.md#migrating-from-agentmixer)).
The [compatibility reference](docs/compatibility.md) covers the TypeScript
package and its `xcb-compat` CLI. [Contributing](CONTRIBUTING.md) ·
[Security](SECURITY.md) · [MIT license](LICENSE)
