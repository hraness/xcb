# Changelog

Release notes for the `v<version>` tag channel. Published GitHub Release
assets, not this file, are the evidence that a version shipped; see
[docs/publishing.md](docs/publishing.md).

Each version's section is headed `## X.Y.Z` (optionally ` - YYYY-MM-DD`) and
holds a summary paragraph followed by a bulleted list of changes. The release
workflow copies that section onto the GitHub Release page and refuses to
publish when it is missing, empty, or still says Unreleased. Write it in the
version bump pull request by renaming `## Unreleased` to the version.

## Unreleased

The first run is shorter and every error says what to do next.

- `xcb accounts login` and `xcb accounts refresh` check the provider
  themselves the first time, so the `Next:` step after `xcb accounts add`
  works without a separate `xcb doctor` run. Account commands accept the
  shortened id the accounts table shows, and say whether no account or
  several accounts matched.
- `xcb setup <provider>` adds an account (or reuses one), checks the
  provider, signs in, and loads models, printing ✓ for each step.
- `xcb --help` groups commands under Start here, Accounts and models,
  Conversations and tasks, Other machines, and Setup and maintenance.
  Internal and machine-only commands are no longer listed.
- `xcb doctor` marks each provider ✓ ready, ⚠ found but not runnable, or
  ✗ missing, and ends with one next step. `xcb sessions` says when there
  are none.
- Errors print as one sentence with the next command to run
  (`✗ No account matches "x".` then `→ xcb accounts`). With `--json`, or
  when an agent runs xcb, errors are a JSON object on stdout. Symbols fall
  back to ASCII when `TERM=dumb` or the locale isn't UTF-8.
- `xcb service install` and `xcb update enable` say, before macOS shows
  its login-item notice, what will open at login and how to turn it off.
- The login service now writes its log to
  `~/Library/Logs/xcb/<label>.log`, and `xcb service` shows that path.
  When the service's latest run ended with macOS refusing access,
  `xcb service` names the Files & Folders setting to turn on for
  Documents, Desktop or Downloads. A service installed by an earlier version keeps
  working; reinstall it to turn on the log.
- The terminal UI honors `NO_COLOR`, and with no accounts it says to
  `/quit` and run `xcb setup claude`.

## 0.8.13 - 2026-09-26

Pinned Claude and Codex builds keep working when the provider updates itself,
reviewed provider builds arrive through a published catalog, and `xcb`
accepts shorter identifiers and longer generate requests.

- A pinned provider now runs from a private copy of its checked executable
  under `providers/bin/`, so a Claude or Codex self-update no longer changes
  the bytes a route runs. xcb re-checks the updated build at daemon start,
  hourly, and before `chat`, `resume`, `run`, or a bare `xcb` launch, and
  switches to it only when it passes the same checks; a rejected build is
  remembered and the previous pin keeps routing.
- `qualified-builds.json` in the repository publishes reviewed
  `(version, sha256)` pairs exact-artifact providers may admit without an
  xcb release. A discovered build nothing yet admits is parked as awaiting
  catalog admission — `xcb doctor` reports it — and a published entry adopts
  on the next hourly pass. A denied digest is rejected outright, and the
  stored catalog is reused when the network is unavailable.
- Devin CLI 3000.11.3 is a supported build, alongside 3000.11.1 and
  3000.10.31.
- Task, conversation, schedule, and fleet device identifiers accept any
  unambiguous prefix, including over the remote command channel.
  `xcb tasks list` lists tasks, and `xcb doctor` reports whether this machine
  is linked to the relay.
- `xcb generate` accepts a `timeoutMs` of up to 300000 (five minutes), up
  from 120000.

## 0.8.11

A linked machine whose session token the relay rejects before it expires now
refreshes the token and carries on, instead of timing out every call until the
token expires.

- After `xcb` opens a controller or a relay lane, the first signed-in call
  checks the session. If the relay rejects it, `xcb` forces one token refresh
  and retries; when a refresh cannot run, the original error is shown.
- The relay can send `xcb link` sign-in codes by email through SendGrid when
  `XCB_RELAY_EMAIL=sendgrid` is set with `XCB_SENDGRID_API_KEY` and
  `XCB_SENDGRID_FROM`. Without it, codes are still written to the relay log.
- The remote operations guide shows how to install a release on a laptop
  before linking it: `XCB_VERSION=<version> ./scripts/install-native.sh`.

## 0.8.10

Remote status no longer reports a healthy but idle fleet as stale.

- An idle relay lane republishes its unchanged status at least every ten
  minutes, and readers mark a lane stale only after twenty minutes without an
  update. `xcb attention --remote --json` includes the same `stale` field.

## 0.8.9

- `xcb daemons` installs named, durable ALGAL processes in a project
  conversation. A daemon persists across restarts, wakes when its inbox
  receives a message or a requested worker task settles, and stops after a
  bounded number of generations. See `docs/plans/effectful-daemons.md`.
- Daemon agent calls never run a provider inline: the daemon suspends on a
  recorded request, the work runs as an ordinary managed task under the
  conversation's project grant, and the daemon resumes only after the
  result is recorded. Stopping a daemon cancels its open child and keeps
  all evidence.

## 0.8.8

[Verified native release](https://github.com/hraness/xcb/releases/tag/v0.8.8)
for macOS ARM64 and Linux x86_64; [public verification run](https://github.com/hraness/xcb/actions/runs/36111915416).

- Fixed a rare false conflict: a file lock xcb had released could briefly
  still look held, because a process spawned while the lock was held can
  carry it through its first moments of startup. xcb now releases its file
  locks explicitly instead of relying on the descriptor close.
- Fixed a rare false `provider command failed`: a provider command that
  closed its output just before exiting cleanly could have its exit rewritten
  as a signal by the cleanup sweep. The runner now waits for the command's
  real exit status before stopping the rest of its process group.

## 0.8.7

[Verified native release](https://github.com/hraness/xcb/releases/tag/v0.8.7)
for macOS ARM64 and Linux x86_64; [public verification run](https://github.com/hraness/xcb/actions/runs/36092507683).

- `/vim` turns on Vim-style modal editing in the composer. Insert mode keeps
  the familiar Codex CLI keys; Esc enters Normal mode with counts, motions
  (`h j k l`, `w b e`, `0 ^ $`, `gg G`, `{ }`, `f F t T` with `;` and `,`),
  the `d`, `c`, and `y` operators, `x X s S r`, linewise or charwise `p` and
  `P`, `J`, and `u` and Ctrl-R for undo and redo. Enter still sends from either
  mode, and the prompt gutter shows `I` or `N`. See the
  [terminal guide](docs/terminal.md).

## 0.8.6

- A signed-in Codex account could not finish connecting: 0.156.1 reports the
  sign-in during the handshake with an `account/updated` notice that xcb read
  as protocol drift and refused. xcb now accepts that notice and the matching
  rate-limit push during connection setup, and tolerates a mid-session
  `account/updated` without failing the turn.
- The Codex boundary receipt's `sandboxFunctionSha256` check now hashes the
  same function slice the probes record, so evidence collection can verify a
  committed Codex receipt instead of always reporting the policy changed.

## 0.8.5

- Codex updates itself, and the only build xcb accepted, 0.155.0-alpha.2.6, no
  longer exists on the machines that ran it or in any public download, so no
  Codex account could take work. xcb now accepts the exact 0.156.1 build. Its
  sandbox checks pass with the same policy as before, and a new offline check
  confirms that the model sees only xcb's tools, that a call to one of them
  reaches xcb, and that nine Codex builtins, including shell commands, patches
  and sub-agents, are refused without running. That check is now part of the
  repository, so the next Codex release can be admitted the same way.
- On 0.156.1, the `ultra` reasoning setting is sent to the model as `xhigh` for
  gpt-6-astra and `max` for gpt-5.6-sol, because ultra's automatic task
  delegation stays turned off under xcb.
- Signed-in coding sessions were confirmed on the previous build and have not
  been rerun on 0.156.1.

## 0.8.4

[Verified native release](https://github.com/hraness/xcb/releases/tag/v0.8.4)
for macOS ARM64 and Linux x86_64; [public verification run](https://github.com/hraness/xcb/actions/runs/36082334475).

- A question, approval, usage limit, or failure that has not changed for a day
  no longer outranks running work. The session grid lists it after active
  sessions, its card shows how long it has waited, and the heading counts it
  separately, as in `2 need attention (5 older)`. Press `2` for active work and
  attention from the last day; `3` still lists all attention, older attention
  last.
- With more than 128 sessions, old attention can no longer push running
  sessions out of the overview: the 128 sessions it shows are chosen by the
  same order.

## 0.8.3

[Verified native release](https://github.com/hraness/xcb/releases/tag/v0.8.3)
for macOS ARM64 and Linux x86_64; [public verification run](https://github.com/hraness/xcb/actions/runs/36073107332).

- Claude Code 2.1.282 reports usage meters for every window in one
  `unifiedWindows` object and no longer sets the single top-level meter xcb
  read, so no usage observation was recorded and an account that had hit its
  limit still showed as unmeasured while routing refused it. xcb now reads
  every reported window, still accepts the older single meter, and records a
  rejected request as that window's exhaustion, so `xcb accounts` shows the
  limit and the retry estimate again.
- Package managers reinstall the Claude binary with group- and world-writable
  permissions (bun's global install does), which failed xcb's executable check
  on the next probe or run until `xcb doctor` ran. Pin verification now
  tightens the mode of an executable you own, as `doctor` already did, and the
  executable checks say which rule failed instead of one combined message.
- On Claude Code 2.1.282 a metadata refresh observes no usage meters, because
  that build reports them only on requests; the next routed turn records them.

## 0.8.2

[Verified native release](https://github.com/hraness/xcb/releases/tag/v0.8.2)
for macOS ARM64 and Linux x86_64; [public verification run](https://github.com/hraness/xcb/actions/runs/36068310101).

- The conversation you have open comes first in the session grid, ahead of
  other sessions needing attention, so the work you just started is never
  pushed off screen. Attention, active, and earlier sessions follow as before,
  and positions still hold while you browse.
- The status line names the account a usage limit belongs to
  (`codex a_7042a73e… quota limited · retry in ~3d`) instead of showing the
  limit beside whichever route is running.
- A session that failed without a response shows its failure reason on its
  card, such as the runtime boundary property that changed, instead of
  "No response yet". Managed tasks show their recorded detail the same way.

## 0.8.1

[Verified native release](https://github.com/hraness/xcb/releases/tag/v0.8.1)
for macOS ARM64 and Linux x86_64; [public verification run](https://github.com/hraness/xcb/actions/runs/36055853293).

- Claude Code 2.1.281 lists its builtin `agents-md` plugin at session start, which
  failed xcb's runtime boundary check and stopped every Claude route on that
  build. xcb now disables that plugin when it launches Claude, so the effective
  plugin set stays empty, and AGENTS.md files are not loaded as instructions
  behind xcb's back. The boundary diagnostic now names the property that
  changed, such as `plugins` or `permissionMode`, instead of a bare mismatch.

## 0.8.0

[Verified native release](https://github.com/hraness/xcb/releases/tag/v0.8.0)
for macOS ARM64 and Linux x86_64; [public verification run](https://github.com/hraness/xcb/actions/runs/36032036107).

- A session grid above chat shows agent names, routed models, activity, and
  response previews with category labels and colors. It grows up to half the
  terminal height, with independent scrolling and a compact view on short screens.
- F6 browses the grid from the keyboard. With `/mouse` enabled, the wheel scrolls
  the panel under the pointer and clicking a card adds an agent reference to the
  draft. References preserve the current chat and task target and send nothing.
- Sessions needing attention come first, followed by active work, with stable
  ordering while responses arrive and while you browse. Keyboard filters show
  all, active, or attention-needed sessions and match names, models, or status.
  Questions and approvals keep their existing controls above the overview.
- Closing a compatibility CLI session store releases its prepared statements
  before the database closes, so an immediate resume can reopen it without
  waiting for garbage collection.
- The route reflex's `judged` head and the task classifier port carry ALGAL's
  generation-1 model-router coefficients: the September 22 fit updated with the
  reflex's anchored learning rule on 84 labeled September 2026 first prompts
  (cross-validated AUC 0.64 on that window against 0.63 for the previous head).
  Routing behavior changes only at the margin; the threshold and kind gates
  are unchanged.

## 0.7.0

[Verified native release](https://github.com/hraness/xcb/releases/tag/v0.7.0)
for macOS ARM64 and Linux x86_64; [public verification run](https://github.com/hraness/xcb/actions/runs/35953085415).

- The terminal uses a quieter prompt and transcript, Markdown and diff styling,
  scrollable help, and searchable command menus. Editing keys, prompt history,
  and the external editor follow familiar Codex CLI behavior.
- Ctrl-T browses saved transcript pages, F3 searches, Ctrl-O copies the last
  answer, and Ctrl-L clears the display. `/resume`, `/rename`, and `/status`
  expose session controls; `xcb history` supports longer paged exports.
- Agent lists update while open. Guidance names its target; ordinary chat
  creates new work. Answers and cancellation check the task revision, queued
  recall refuses started work, and submissions preserve their intended context.
- Private input journals retain drafts, image references, prompt history, and
  requests awaiting acknowledgement across restarts. Recovery never resends
  input automatically. See the [terminal guide](docs/terminal.md).

## 0.6.0

[Verified native release](https://github.com/hraness/xcb/releases/tag/v0.6.0)
for macOS ARM64 and Linux x86_64; [public verification run](https://github.com/hraness/xcb/actions/runs/35939241299).

- Resumable ALGAL controllers can request bounded ordinary worker tasks with
  `xcb backlog program` or `xcb schedules program --managed-calls`. Each child
  uses normal routing, project authority, budgets and approval handling.
- Durable checkpoints and deterministic child identities survive restart.
  Waiting controllers release their worker slot; only conclusively completed
  children can supply results. Cancellation and uncertain execution retain the
  existing settlement safeguards.
- `xcb backlog program-status` and the TUI's `/program` expose linked child
  status, call progress and execution receipts. Existing pure planners remain
  compatible.
- Settle and confirm reflexes gain local auto-certification from observed user
  replies, with explicit confidence thresholds, holdouts and rollback. Existing
  authority gates and risk vetoes still apply.

## 0.5.0

[Verified native release](https://github.com/hraness/xcb/releases/tag/v0.5.0)
for macOS ARM64 and Linux x86_64.

- Durable task steering and explicit completion subscriptions through
  `xcb steer`, `xcb watch`, and their TUI commands. Stable identities make
  retries idempotent; inter-agent messages share the same bounded inbox.
- `xcb inbox` and `/inbox` expose delivery history, full event inspection,
  pagination, and distinct waiting, queued, prepared, delivered, held and closed
  states. Delivery requires the exact submitted prompt and a settled receipt.
- Coalesced input batches survive restarts and late arrivals. Inbox-driven
  continuation preserves approval, authority, cancellation and budget gates,
  uses a neutral handoff, and does not train the continuation reflex.
- Reflexes v2 adds fitted unfinished-work and confirmation heads, live metrics,
  and challenger promotion through forward trials. The settle reflex observes
  by default; acting and routine confirmation require separate opt-ins.

## 0.4.0

First native release line. Earlier `v0.1.0`–`v0.3.0` releases are AgentMixer
compatibility packages only.

- Native Rust CLI (`xcb`) for Claude, Codex, and Devin subscriptions: account
  custody, model selection, and usage in one local terminal workspace, with
  `doctor` admission of the exact provider builds.
- Managed conversations: sessions with bounded workspace tools (list, read,
  search, write, mkdir, remove, rename with revision checks), stored under the
  private state root.
- Isolated command runner for tests, builds, and filtered Git on macOS ARM64,
  qualified against its VM boundary suite and offline Cargo/Bun caches.
- Updater: `xcb update` policies (`notify`, `auto`, `disable`), a daily macOS
  LaunchAgent, and `xcb upgrade` through the checksum-verified installer.
- Native release assets: `xcb-<version>-<os>-<arch>.tar.gz` plus `.sha256`
  for Ubuntu and macOS, attached to the immutable GitHub Release.
- TypeScript compatibility package renamed from `@hraness/agentmixer` to
  `@hraness/xcb`; its CLI installs as `xcb-compat` so it never shadows the
  native binary.

Unqualified in this release: Codex and Devin execution outside macOS Seatbelt,
Linux Codex and Devin execution paths, and the compatibility CLI's Codex and
Devin task routes, which stay disabled pending exact-runtime qualification.
A successful `doctor` does not prove a working coding session.
