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

Unattended schedules can stop waiting on unanswered tasks after an interval
the owner chooses.

- `xcb schedules add`, `program`, and `edit` accept `--settle-unanswered-after
  <seconds>`. The setting is off by default, saved with the schedule, and
  visible in `show` and JSON output. Set it to 0 when editing to turn it off.
- When the interval passes without an answer and no worker is still running,
  the task fails with a recorded reason, without a retry. Waiting programs
  settle and later schedule wake-ups can proceed. A reply starts a new wait;
  uncertain and running work is unaffected.
- Correct the documented disk warning, pause and resume defaults to 24, 8,
  and 12 GiB.

## 0.20.7 - 2026-10-09

A slow native build no longer costs an unattended task its work.

- Workers are now told to size `workspace_native_exec` timeouts to the
  command, up to 600,000 ms for builds and test suites, and to split long
  work into build and test steps. The old instruction to keep every native
  command at or below 120 seconds made cold builds time out, which left the
  whole task with uncertain effects and got scheduled runs dismissed before
  they opened a pull request.
- `workspace_native_exec` accepts an optional `githubCredentials`. `false`
  runs the command without the grant's GitHub credentials; `true` requires
  them.
- When a native command reaches its timeout, xcb stops its process group and
  confirms every process exited. If the command held no GitHub credentials,
  or was a read-only `gh` query such as `gh pr checks --watch` or
  `gh run watch`, the worker gets a settled result with `"status":
  "timed_out"`, the output so far, and its workspace file changes kept, and
  the task continues. A timed-out command that held credentials, a cancelled
  command, or one whose processes xcb could not prove gone still reports
  uncertain effects.

## 0.20.6 - 2026-10-09

Native build and test commands now document the time needed for cold builds.

- `workspace_native_exec` tells workers that builds and test suites may take several minutes and accepts a `timeoutMs` up to `600000ms`, including in its compact descriptor.
- Regression coverage pins the full and compact tool descriptions and the timeout schema.

## 0.20.5 - 2026-10-08

A scheduled program no longer waits forever on a child task you cancelled.

- When a program's child task is cancelled, for example with `xcb tasks
  cancel` while it waits for your input, the program now stops with
  "program stopped because child … settled as cancelled", the same way it
  stops for a failed child, and its schedule wakes again. Programs already
  stuck on a cancelled child stop the next time the service checks them.
- Cancelling a program that waits on a cancelled child now cancels it on the
  service's next check instead of leaving it at "cancellation requested".

## 0.20.4 - 2026-10-08

Claude accounts now show their email in `xcb accounts`.

- `xcb accounts` and `xcb --json accounts` show a Claude account's email in
  place of the `claude/a_…` placeholder once xcb sees it. xcb reads it during
  `accounts login`, `accounts refresh`, and ordinary runs, from Claude Code's
  startup report, the profile Claude Code writes inside xcb's private launch
  folder, or the email confirmed at browser sign-in. None of these sends a
  prompt. Your plan name is never changed, and an email that is empty,
  untrimmed, longer than 320 characters, missing `@`, or contains control
  characters is ignored instead of failing the run.
- The Claude profile is now read even when Claude Code writes it with its
  default file mode, which previously hid the email.

## 0.20.3 - 2026-10-07

Native commands in different workspaces no longer block each other.

- The supervisor's in-process writer lock is now scoped to one workspace. A
  long native command in one workspace no longer makes native commands in
  every other workspace fail after five seconds with "workspace writer is
  busy; retry", which stalled concurrent unattended schedules. One workspace
  still runs one writer at a time (#480).

## 0.20.2 - 2026-10-07

Signing in loads the account's models, and native grants can let workspace
commands read and run host toolchains.

- `xcb accounts login`, `accounts token` and `accounts import-codex` now load
  the account's model catalog as their last step, as `xcb setup` and `accounts
  refresh` already did. A sign-in whose load fails still stores the credential
  and names `xcb accounts refresh` as the next step. Their `--json` output adds
  `models` (the count loaded, or `null`); `generate --capabilities` is
  unchanged and still reads local metadata only.
- `xcb native grant --host-read` lets a workspace's native commands read and
  run host toolchains anywhere outside private state, as a provider's own
  workspace-write mode does, and puts Cargo, Bun, nvm Node and Homebrew on
  their `PATH` with rustup pointed at the host's toolchains. Writes stay
  confined, and credentials, `~/.config`, shell history and personal
  `~/Library` stores stay hidden. Existing grants are unchanged.

## 0.20.1 - 2026-10-07

Signed-in accounts stay available to applications however long ago xcb last
observed their model catalog.

- `generate --capabilities` no longer requires a model to have been seen in the
  last 24 hours before listing it as `pending`, and `generate` no longer refuses
  the first automatic check of such a model. Upgrading to 0.20.0 over an older
  state folder reported every account `models_unavailable` with no models until
  `xcb accounts refresh` ran; the last observed catalog is now offered in
  catalog order within the existing caps, and a model the provider withdrew
  fails its own request as before. `models_unavailable` now only means the
  account's catalog has never been observed. `--capabilities` still reads local
  metadata only and never refreshes a provider.

## 0.20.0 - 2026-10-07

Apps can use a signed-in account with no manual command. The first
`xcb --json generate` for an account and model checks it automatically, and
checks again after xcb, the provider, the settings or the sign-in changes.

- Check the provider's sandbox on this computer automatically, once per xcb
  build and provider, without credentials. If it can't be confirmed, apps can't
  use that provider (`sandbox_unproven`); nothing runs unsandboxed.
- Run the fixed harmless challenge automatically on the first `generate`, one
  per account at a time, and record only identities, the result and a time.
- Add `admission` (`pending`, `admitted` or `qualified`) to `--capabilities`
  account and model rows. `pending` accounts are available; their first call
  takes up to 60 seconds longer.
- Add `xcb application disable|enable [--account ID]` and
  `xcb application status`, which turn app access off globally or per account.
- Keep `qualify-application --evidence` as an optional stronger qualification.
- Replace the `application_not_qualified` reason with `application_disabled`,
  `sandbox_unproven` and `admission_failed`, and document the existing
  `authentication_required`.

## 0.19.9 - 2026-10-07

xcb keeps unattended schedules running without you.

- `xcb schedules edit <id> --dismiss-uncertain true` lets a schedule dismiss uncertain work in its directory once no worker is still running, the same way `xcb backlog dismiss` does. Nothing is retried, and the next wake-up goes ahead instead of waiting for you.
- After an xcb upgrade, a newly adopted provider build, or 30 days, xcb re-runs the native checks you already passed for providers a directory grant names. Granted directories keep running instead of quietly finding no account.

## 0.19.8 - 2026-10-06

xcb lets a herd program move on after you dismiss one of its tasks.

- A program waiting on a task you closed with `xcb backlog dismiss` now stops, or finishes cancelling, instead of waiting forever. This includes tasks dismissed with 0.19.7.
- Record dismissals on the task itself, so later xcb versions can tell them apart from other failures.

## 0.19.7 - 2026-10-06

xcb keeps unattended workers moving when a native command fails or a worker ends with an uncertain result.

- Return a native command's exit status, output and error output when it fails, instead of marking the task uncertain. Only a timeout, cancellation or unconfirmed process exit is still treated as uncertain.
- Clip long native command output instead of discarding it.
- Release the account and project as soon as an uncertain worker's processes have exited. The task stays uncertain and is not retried.
- At startup, release runs left behind by a stopped xcb process, using the same checks as `xcb recover --yes`.
- Add `xcb backlog dismiss` to close uncertain work you have checked yourself. It is marked failed and is not retried, so it no longer stops the project's schedules.

## 0.19.6 - 2026-10-06

xcb keeps unattended herds moving when provider tool calls exceed their declared bounds.

- Clamp oversized backlog inspection requests to the bounded 64-item page instead of stranding a worker turn.
- Guide managed Codex workers toward short native commands and preserve uncertainty after interrupted execution.
- Add regression coverage for oversized backlog input.

## 0.19.5 - 2026-10-06

xcb keeps Codex-backed unattended herds moving when display-only protocol updates arrive during initialization or after a managed turn.

- Accept bounded, thread- and turn-bound display observations across the provider turn lifecycle without treating timing as a fatal protocol error.
- Keep native execution, filesystem changes, reroutes, authentication recovery, and provider process events fail-closed.
- Add regression coverage for pre-turn and post-turn informational notifications.

## 0.19.4 - 2026-10-06

xcb keeps Codex-backed unattended herds moving when display-only protocol updates arrive during a managed turn.

- Accept bounded, thread-bound plan and lifecycle observations without treating them as executable authority.
- Keep native execution, filesystem changes, reroutes, authentication recovery, and provider process events fail-closed.
- Restore a publishable changelog section so the verified release workflow can complete.

## 0.19.3 - 2026-10-06

xcb keeps unattended herds moving when host pressure or verbose worker reports would otherwise strand them.

- Use a throughput-oriented disk reserve and reclaim only proven disposable caches and stale browser code-sign clones.
- Compact oversized managed-worker handoffs while retaining the exact child report receipt and digest.
- Harden unattended maintenance and recovery evidence without weakening custody or approval gates.

## 0.19.2 - 2026-10-05

xcb keeps a durable supervisor under custody while it reconciles large state roots at startup.

- Extend the bounded pre-heartbeat startup grace so launchd does not restart a valid supervisor before reconciliation can finish.
- Preserve the existing stale-heartbeat restart and worker-recovery rules after startup.

## 0.19.1 - 2026-10-05

xcb stops selecting Claude accounts after an explicit organization refusal of
Claude Code subscription access.

- Record this provider error as an account-access failure that survives restart,
  so unattended work can use another available account.
- Keep generic errors and quoted refusal text from blocking an account.
- Keep the affected account unavailable until access is repaired. Signing in
  again may be insufficient when an organization administrator must enable access.

## 0.19.0 - 2026-10-05

xcb adds project herding controls and persistent recovery for unattended work,
with local evidence for account health, execution, and delivery.

- Add project concurrency and task-rate controls, schedule inspection, and
  offline checkout preflight with revision, input, and budget checks.
- Enforce hourly limits for automatic program and daemon children without
  losing queued work or charging budgets twice.
- Recover from temporary quota limits and explicit settled provider outages
  with persistent backoff, account cooldowns, and one recovery trial at a time.
- Scale concurrency from runnable work and available account capacity, with
  bounded growth and reductions under host pressure.
- Preserve required follow-up work and routine project-authorized continuation
  across worker responses rather than treating every final response as delivery.
- Add optional xAI, Vercel AI Gateway, and compatible-endpoint judges with
  provider-specific credentials; direct xAI uses Grok 4.7 by default.
- Add native execution grants, read-only status and custody inspection, exact
  tested-artifact qualification, and explicit workspace grant revocation.
- Retain scope, account, process-cleanup, and uncertain-effect checks. Provider
  access and month-long live operation require separate validation.

## 0.18.0 - 2026-10-04

xcb's current source build is agent-first: use the headless CLI, JSON route,
and SDK instead of the former interactive terminal or hosted remote commands.

- Remove the Ratatui frontend and native chat, resume, and remote entry points.
  Saved conversations and task history remain readable through local commands.
- Add `xcb conversations --new --json` for creating project views without a UI.
- Add `xcb usage`, which shows token use across your coding agents by day,
  agent, provider, and model from aicharts' record on your computer. It never
  uploads, and xcb forwards only aicharts' report and scheduling commands.
- `xcb usage connect` gives Claude and Codex tasks aicharts' read-only usage
  tools through xcb's host tool bridge, pinned to the installed aicharts build;
  `xcb usage disconnect` removes them. `xcb doctor` now reports whether aicharts
  is collecting and whether that pin still matches.
- `curl -fsSL https://xcb.sh/install.sh | sh` also installs aicharts 0.3.1 on
  macOS (Apple silicon) and Linux x86_64, checked against a pinned digest and,
  on macOS, its Developer ID signature, and turns on local usage history on a
  first install. `XCB_AICHARTS=no` skips it; `XCB_USAGE_HISTORY=no` leaves
  history off.
- Keep inbox replay, project isolation, managed-program restart, and cancellation
  checks on the headless path; preserve the current website and its checks.
- Define the north-star execution plan with ALGAL, protocol and SDK milestones,
  and a gated Valhalla transport direction. Valhalla syncing is not shipped.

## 0.17.11 - 2026-10-02

Real Codex turns no longer die on an internal call budget far below the
provider's own admission bound.

- The per-turn unique tool-call cap rises from 128 to 1024, matching the
  Codex adapter's `MAX_CALLS` admission limit. A managed turn doing real
  work (bounded file edits and command calls) was being settled uncertain
  after ~130 calls; the duplicate-identifier rejection and every other
  protocol guard are unchanged.

## 0.17.10 - 2026-10-02

Aggregate host heartbeats now power a compact activity view with task and resource signals.

- Heartbeats report bounded task-state counts, memory pressure, swap, physical memory, and disk-free summaries.
- The public status surface adds dense activity bars and resource badges without task titles, paths, IDs, or project details.

## 0.17.8 - 2026-10-02

Command snapshots stop rejecting entire workspaces over committed files that
are too large or hardlinked — they are excluded with an explicit report — and
managed Codex tasks gain a strictly admitted host-execution lane for the
repositories that still need real network and GitHub credentials.

- `workspace_exec` snapshots now exclude regular files over the per-file byte
  bound and files with extra hard links instead of failing the whole
  workspace, recording one bounded report of the excluded paths. Publication
  still conflicts a guest-authored output that collides with an excluded file,
  so a worker can never silently clobber content it never saw.
- New `workspace_host_exec` runs a bounded command in the real worktree with
  host networking only when the trusted host process names the exact task:
  `XCB_HOST_CREDENTIALS_TASK` must equal the persisted task ID, the session
  must be managed Codex in an owned run, and the workspace binding must match.
  `gh` token, Git author identity, and `SSH_AUTH_SOCK` are read on the host at
  execution time, staged through a private mode-0600 environment file consumed
  before launch, zeroized, and scrubbed from captured output. Commands that
  fail, cancel, or time out retain uncertain effects and account custody and
  are never silently retried.
- Codex protocol failures now carry evidence instead of opaque labels: an
  unreconciled `tokenUsage` reports the provider's own counter values through
  a typed `CodexUsage` error, and the ambiguous "duplicate or excessive tool
  call" split into "duplicate tool call identifier" and "tool call limit
  exceeded".

## 0.17.7 - 2026-10-02

Claude browser sign-in now completes through the provider's own loopback
handoff instead of hanging at a code prompt, and account refresh reports
real remaining quota for Devin and OAuth-signed Claude accounts.

- `xcb accounts login` no longer suppresses the provider's browser launch:
  Claude's sign-in child opens its own `redirect_uri` pointing at its local
  callback listener, so completing the page finishes the login. The printed
  link is the manual fallback variant whose page cannot reach that listener,
  so it is shown for headless setups but no longer auto-opened. A pasted
  code that lands before the provider's input reader mounts is redelivered
  on a bounded cadence, and the CLI prints a heartbeat while the exchange
  runs instead of sitting silent.
- `xcb accounts refresh` now reports Devin plan quota: the refresh probe
  reads the provider's own account status and records daily and weekly
  remaining-quota windows with their reset times and the account's email and
  plan name, so listings show real remaining percentage instead of an
  unmeasured meter. An exhausted window the service omits (a reset timestamp
  with no remaining percentage) is recorded as fully used.
- Claude accounts signed in through the browser OAuth flow fall back to the
  provider's usage endpoint when the in-band `get_usage` reply carries no
  windows; setup-token accounts remain metered by observed rate-limit events.

## 0.17.6 - 2026-10-02

Supersedes 0.17.5, whose tag was created before its changelog sections were
deduplicated; no 0.17.5 release was published.

Managed tasks can pin an exact model, and provider self-updates are adopted
automatically when their reviewed runtime contract is available, so routine
Claude, Codex, and Devin updates no longer require an xcb release.

- `xcb backlog add --model provider/model[/effort]` pins a managed task to
  one observed model. The pin resolves to a canonical key at admission
  (unobserved, ambiguous, and `routing never`-excluded values are refused),
  implies the model's provider as required, and is stamped on every worker
  session so failover stays inside the pin.
- Worker agents can pin the same way with `xcb_backlog_add`'s optional
  `model` field.
- A queued task whose pinned model later lands on `routing never` fails at
  dispatch instead of waiting on a condition nothing lifts.
- Claude keeps its supported major-version floor while honoring reviewed
  catalog denials.
- Codex and Devin accept exact reviewed `(version, sha256)` catalog entries
  without waiting for a new xcb binary release; unknown or denied builds stay
  blocked and the last working pin continues routing.
- `xcb doctor` refreshes provider admission during normal use, and login,
  metadata, and task launch paths share the same admission check.

## 0.17.4 - 2026-10-02

Accounts can run more than one task at a time.

- `max_runs_per_account` in `config.json` (default 1, up to 32) sets how
  many tasks may share one subscription at once; every run keeps its own
  provider profile while sharing the account's quota and rate limits.
- Sign-in, credential checks, and other account operations still take the
  account alone: they wait for running tasks to settle, and no task starts
  while one holds the account.
- Routing and failover now count an account's live runs instead of treating
  any busy account as unavailable, and the no-route notice says when an
  account is at its run limit.
- The local database migrates on first open to schema version 2; xcb builds
  before this release refuse it, so old and new versions cannot interleave
  account writes.

## 0.17.2 - 2026-10-01

Browser sign-in waits until Devin is ready for the code and reports rejected
Claude codes clearly.

- Drain the private sign-in terminal so Devin can finish its terminal setup
  and receive a pasted code on macOS.
- Give the provider terminal a usable window size and wait for Devin's actual
  code field before requesting input.
- Stop failed Claude sign-in attempts and explain when a code is incomplete
  or rejected, including when the helper remains open.
- Keep provider terminal output private and stop attempts that exceed the
  output limit, while confirming process cleanup before releasing an account.

## 0.17.1 - 2026-10-01

Health checks keep a working provider available when the system has a newer
build that xcb has not checked yet.

- Keep the saved, verified provider executable when `xcb doctor` discovers an
  unsupported replacement, including one supplied with `--executable`.
- Report the skipped build and the version xcb will continue to use.
- Recheck the saved executable before retaining it; a changed or unsupported
  saved build cannot qualify a replacement.

## 0.17.0 - 2026-09-30

Chrome setup can authorize Claude browser access through full sign-in on
macOS, with refresh credentials in a dedicated xcb Keychain entry.

- Open full Claude sign-in from `xcb tools setup-browser` when needed, or
  explicitly with `xcb accounts login NAME --browser`.
- Refresh short-lived Claude credentials before use and preserve an existing
  account's credentials until replacement sign-in is verified.
- Recover interrupted full sign-ins after their processes have stopped,
  retaining saved credentials and requiring sign-in again when needed.
- Distinguish missing token scope, rejected credentials, provider outages, and
  connection failures instead of a generic extension-connection message.
- Preserve model credentials and existing tool configuration when browser
  authorization fails, and release the setup account when cancelled before
  the browser server starts.
- Keep model-only Claude sign-in available for accounts without a shared
  browser connection.

## 0.16.4 - 2026-09-30

Browser sign-in accepts pasted codes reliably and restores the terminal after
completion, cancellation, or failure.

- Keep Devin code entry in xcb and send the code to a private provider terminal,
  preventing sign-in from stopping when the provider reads a foreground terminal.
- Normalize Enter, editing, and Ctrl+C for Claude, Devin, Codex, setup, and relay
  prompts. Hide pasted sign-in codes and restore the exact previous settings.
- Let the provider finish exchanging a submitted code after code entry ends,
  and show when xcb is finishing sign-in.
- Keep cancellation active from provider preparation through process cleanup.
  Preserve saved accounts and recovery files when cleanup cannot be confirmed.
- Restore terminal settings around compatibility CLI sign-in and logout. Bound
  child waits and stop Claude fallback attempts after cancellation.

## 0.16.3 - 2026-09-30

Verified native release installs and supported global compatibility installs
keep xcb current before interactive commands, with an opt-out and checks that
protect pinned tools and running work.

- Enable native automatic updates only for unpinned release installs whose saved
  paths, version, and executable and installer checksums still match. Preserve
  saved notification-only and disabled preferences.
- Check at most once a day before interactive commands and start the updated
  binary before running the requested command. CI, JSON, noninteractive uses,
  and `HRANESS_NO_UPDATE=1` skip automatic checks.
- Wait for other xcb commands, supervisors, and services to exit before
  replacing the binary. Updates do not stop work or restart services.
- Keep source builds and package-managed native installs on their existing
  update process. Refuse mismatched install records, mutable releases, and
  implicit downgrades; preserve exact-version pins.
- Add `xcb-compat update`, `check`, `status`, `enable`, and `disable`. Supported
  Bun/npm global copies use the immutable compatibility archive and its digest,
  with install scripts disabled and the SDK unchanged.
- Keep help and version output free of updater state changes. Protect an
  interrupted native install even when the updater parent stops before its
  installer, and verify the new executable before reporting success.

## 0.16.2 - 2026-09-30

Claude and Devin can hand tasks that need native desktop control to Codex,
which runs the installed computer tools with automatic approval review.

- Add `xcb run --desktop` and let agents request desktop control when they
  discover that a task needs it. Keep the task's conversation through a safe
  handoff to Codex, preferring Astra when no model is pinned.
- Preserve browser and desktop requirements through quota failover, retries,
  and resumed work, including when no matching account is currently available.
- Check that the installed connector supports native desktop control before
  starting a desktop task. Shared browser tools do not satisfy that check.
- Keep tasks on Codex after a native tool call without treating every native
  call as a request to use a signed-in website.

## 0.16.1 - 2026-09-30

Mac releases preserve the submitted binary when Apple notarization takes longer
than the CI wait, so failed-job retries can finish the original submission.

- Save the signed candidate and its submission record before waiting for Apple,
  after removing temporary signing credentials.
- Resume finalization with the original artifact and submission ID. Verify the
  release identity and hashes before requiring Apple's acceptance and checking
  its online notarization ticket.
- Refuse repeated signing or submission when an earlier attempt ran. Keep
  pending candidates separate from publishable release assets.
- Document initial notarization delays, the failed-job retry procedure, and
  recovery limits when submitted bytes are unavailable.

## 0.16.0 - 2026-09-30

Tasks that need an existing signed-in browser stay with Codex and prefer
Astra. Shared host tools add browser and computer capabilities through each
provider's tool bridge.

- Keep signed-in-browser requirements and explicit route choices across
  retries, resumed work, and safe handoffs from Claude or Devin. Ordinary
  browser tests and login-code work continue to use normal routing.
- Register checked MCP servers for all three providers. Share screenshots
  through private session attachments, including after a provider handoff.
- Connect Codex's installed desktop computer-use plugin with its automatic
  approval reviewer. Connect Claude's Chrome extension through a selected
  xcb Claude account, without sharing that account's credentials with other
  providers.
- Track host tool processes and browser tabs through cancellation and
  recovery. Approval denials stop work without retrying another provider.
- Fix Mac release signing and installation checks to use Apple's required
  keychain lookup and signature-verification syntax.

## 0.15.2 - 2026-09-30

Mac release builds use a stable Apple Developer ID identity so updates can
retain the application's identity in macOS permission checks.

- Sign Mac release binaries with the `dev.hraness.xcb` identifier, a Developer
  ID Application certificate, hardened runtime, and Apple's timestamp service.
- Require Apple notarization before publishing Mac release archives. Signing
  credentials are available only to the release signing job.
- Verify the expected Apple signing identity before running or installing a
  downloaded Mac binary. Source builds and explicitly selected older releases
  keep their existing installation paths.
- Document the one-time installer update and the separate permission test
  needed when moving from an ad hoc build to a Developer ID build. The first
  signed installation can require another macOS approval.

## 0.15.1 - 2026-09-30

Workspace metadata checks no longer pause the supervisor when a filesystem
open stops responding.

- Check workspace metadata in one background thread at a time, so heartbeat
  updates, task cancellation, and resource samples can continue during a slow
  filesystem or operating-system access check.
- Read Git metadata only from verified regular files. Reject symbolic links,
  special files, oversized content, and objects replaced during the read.
- Apply discovered repository names only while the workspace directory and
  its saved registration still match the original observation.

## 0.15.0 - 2026-09-30

Unattended hosts can pause new work under resource pressure, recover a stalled
supervisor, and report a small heartbeat to an external status page.

- Added `xcb resources` and an opt-in guard for sustained memory pressure,
  low state or workspace disk reserves, and failed or stale measurements.
  Existing work can still finish or be cancelled while launches wait.
- Added a login-service watchdog that verifies the supervisor it starts,
  detects stalled progress, and limits diagnostic logs to 6 MiB. Service
  status distinguishes installation from a running, watched supervisor.
- Added a host maintenance runner with independent sampling, optional
  recurring Codex reviews, pinned tools, and retained uncertain outcomes.
  Cleanup and task cancellation retain the host's permission and ownership
  checks.
- Added optional five-minute heartbeats with separate credentials, replay
  protection, bounded retries, and only two latest anonymous status records.
  The status endpoint reports receipt age without exposing task or account data.

## 0.14.5 - 2026-09-30

Claude and Devin sign-in show the browser link in the terminal so you can
open it yourself when browser handoff fails.

- Claude recognizes terminal hyperlinks and prints the sign-in URL before
  asking for an optional code.
- Devin uses its supported browser-and-code sign-in flow, which prints the
  link and asks for the code from the sign-in page.

## 0.14.4 - 2026-09-29

Claude and Codex start with automatic approval review. xcb stops after a
denied action, including when Claude reports the same turn as successful.

- Claude starts in Auto mode, and Codex starts with its automatic approval
  reviewer. xcb verifies the effective mode before allowing work and refuses
  a provider that changes it. The private profiles, workspace tools, and
  operating-system restrictions stay in force.
- A structured Claude permission denial stops automatic continuation and
  provider switching, including when the provider reports a successful
  result. The notice does not copy denied tool inputs into diagnostics.
- Devin verifies its effective approval mode and prevents a rejected tool
  call from being approved later. Its existing automatic workspace approvals
  remain in use; bypass did not pass the native tool-boundary checks.
- Credential-free native probes check approval modes and delegated-agent
  behavior against pinned provider builds. Native delegation remains disabled;
  reported child activity cannot finish the root turn.

## 0.14.3 - 2026-09-30

Claude and Devin account setup now guide you through browser sign-in in the
terminal. Sign-in links stay visible when the browser does not open.

- Claude receives the terminal input its sign-in command requires. xcb opens
  the sign-in page, displays the link, and accepts an optional browser code
  without showing that code as you type.
- `xcb accounts add devin`, `xcb setup devin --new`, and
  `xcb accounts login <account>` use Devin's browser login in a private profile.
  Setup stores the sign-in in the selected account and loads its models.
- Devin setup offers the same existing-account and new-account choices as
  Claude and Codex. Reconnecting an account with rejected credentials repairs
  sign-in; an already connected account cannot silently change identity.
- Cancellation waits for the sign-in process to stop before releasing the
  account. Temporary login profiles are removed after confirmed cleanup.

## 0.14.2 - 2026-09-30

Adding a Claude or Codex account in a terminal now takes you through sign-in
and model loading without copying an account ID. Pickers show your current
choice and explain what to do when their lists are empty.

- `xcb accounts add claude` and `xcb accounts add codex` finish account setup
  in an interactive terminal. JSON and piped commands still only add a record.
- `xcb setup` offers existing accounts and an option to add another. Use
  `--new` to add directly or `--account` to finish a specific account’s setup.
- Account, model, session, conversation, and pane pickers open on the current
  choice when available. Empty lists and filters with no matches show next steps.
- Selecting a turned-off account explains how to enable it.
- Codex sign-in shows the device code and copies it on macOS when clipboard
  access works, then waits for Enter before opening the sign-in page. If either operation
  fails, you can use the displayed code and link.

## 0.14.1 - 2026-09-29

Automatic continuation now reads how a turn ended and answers the way you
would: it sees in-flight work through, hands back steps the worker could
have done itself, takes the worker's own recommendation, and leaves only
the steps that need you. An application's approval to use an account and
model now lasts until something it covers changes instead of 24 hours.

- A completed turn that describes work still in flight is told to see it
  through; one that waits for you to run a merge, a rerun, a push or a
  cleanup the worker's own tools perform is told to do it itself; a question
  the worker answered with its own recommendation is answered "go with your
  recommendation". A turn that hands you a step only you can take (a
  sign-in, a one-time code, a Keychain or password prompt, a secret, a
  payment, or a fact only you know) is never continued or answered.
- A question the worker asked in words alone now reaches the settle heads,
  so a routine "should I open the PR?" no longer stops a task; a denied
  provider request still does.
- Direct sessions and the terminal continue the way managed tasks do: a
  turn the certified settle reflex reads as stopped short, or as a routine
  request for a go-ahead, continues on its own within the same gates.
- The continuation budget is eight turns in a row within an hour (was three
  within ten minutes), and managed tasks get nine attempts (was four).
- A configured judge only vetoes a continuation; it no longer starts one.
- "Can't you do this yourself?" and "stop asking" replies now teach the
  ledger that the turn stopped short.
- An application's approval to use an account and model no longer runs out after 24 hours. It lasts until the xcb binary, provider build, platform, application settings or account sign-in changes, so a scheduled application no longer stops each day until the tests are rerun and the live check repeated. A model approved this way also stays usable without a daily catalog refresh. `qualification.expiresAt` in `generate --capabilities` is now always `null`.

## 0.14.0 - 2026-09-29

Automatic routing now follows a preference stack you can edit: for each
kind of task, an ordered list of provider/model/effort patterns says which
models xcb prefers, the newest release of a model family wins within a
pattern, and Devin serves as a fallback. xcb now runs GPT-6-Sol on Codex.

- Managed tasks, unpinned `xcb run`, `xcb --json route` without a model
  pin, and continuation after a usage limit order the routes that can take
  the task by the `routing` stack in `config.json`: build-outs prefer Astra
  at ultra, then Fable at max; large tasks prefer Astra at max, then Fable;
  most tasks prefer Sol at ultra, then Opus at max; mechanical tasks prefer
  Sol at max, then Opus. A pattern such as `codex/gpt-*-sol/ultra` matches
  every Sol release and the newest wins, so a new model needs no
  configuration change. The route reason names the tier and the pattern
  that decided.
- `routing.never` lists routes xcb never uses, including by an explicit
  `--model` pin, which is refused instead of widened; the default excludes
  SWE models on Devin, which also leave the built-in favorites.
  `routing.fallback_providers` (default: Devin) names providers used only
  when no other provider can take the task now.
- `xcb routing show` prints the effective stack with the observed models
  each pattern matches; `xcb routing never add|remove <pattern>` edits the
  exclusions. Sol 6.x releases are now recognized model families.
- GPT-6-Sol (`gpt-6-sol`) on Codex CLI 0.159.0 passes the tool, callback, and
  sandbox checks at every reasoning level, so `xcb models` lists it and routing
  can pick it. GPT-6.1-Sol follows once a stable Codex build bundles it; Codex
  0.159.0 does not.

## 0.13.1 - 2026-09-29

xcb now runs Codex CLI 0.159.0, so Codex accounts take tasks again on
machines that upgraded Codex, and subscription rotation at usage limits
covers all three providers.


xcb now runs Codex CLI 0.159.0.

- Codex CLI 0.159.0 on macOS ARM64 passes the executable, tool, configuration,
  and sandbox checks, so `xcb doctor` no longer reports it as a build xcb can't
  run. The previous supported builds, 0.158.0, 0.157.1, and 0.156.1, remain
  supported.
- When Codex ends a turn because its own review step refused the turn's
  actions too many times, xcb records a policy failure. It does not count the
  failure against the account's usage limit, and it does not move the task to
  another account.

## 0.13.0 - 2026-09-29

xcb now rotates subscriptions when a provider reports a usage limit. An
account whose limit came without a reset time stays at a known limit for a
configurable cooldown instead of being routed to again, and failover picks
its next route with the same rules as automatic routing, rotating equal
accounts least-recently-used first.


- The GitHub release step checks each uploaded file once instead of downloading every uploaded file again after each upload, which made a failed GitHub API call likely once releases carried eleven files.
- An account whose provider refuses a turn for its usage limit without saying when the limit resets (every Codex `usageLimitExceeded` and Devin resource-exhaustion error, and a Claude rejection without a reset time) now stays at a known usage limit for `quota_limit_cooldown_ms` (default 30 minutes, 1 minute to 7 days in `config.json`) instead of reading as ready and being routed to again. Automatic routing, `xcb --json route`, other terminals, and `xcb accounts` (`quotaBlockedUntilMs`) all see the limit; a reset the provider reports later replaces it, even when sooner. Limits on one model only are still not recorded against the whole account.
- Failover after a usage limit in `xcb run` and the terminal now picks routes with the same rules as automatic routing, so accounts without a usage meter (Devin) or with a reading older than five minutes are targets; it prefers the same model on another account, then the same provider, then other providers, and rotates equal accounts least-recently-used first. It is no longer cut off by the auto-continue time budget, and when no account can take the task it says which accounts are at a limit, signed out, or busy, and the earliest known reset. See `docs/failover.md`.

## 0.12.0 - 2026-09-29

xcb now ships for Windows x86_64 and Linux ARM64. On Windows, providers run
through the Linux build in WSL2; on Linux, a release install can set up
Claude's sandbox by itself and run as a systemd user service.

- xcb now compiles for Windows x86_64 (`x86_64-pc-windows-msvc`), and CI builds and tests it on Windows Server 2025 on every Rust change as a required check. State lives in `%LOCALAPPDATA%\xcb`, private files and folders get an owner-only access list instead of Unix permission bits, and junctions and symlinks are refused where Unix refuses symlinks. Running or signing in to Claude Code, Codex, or Devin, daemons, and the workspace tools are refused on Windows with a message that points to the Linux build in WSL2.
- Releases now include a Windows x86_64 build, `xcb-<version>-windows-x86_64.zip`, with a checksum and build provenance like the other archives. It is not code-signed, so SmartScreen may ask before its first run. `irm https://xcb.sh/install.ps1 | iex` installs it to `%LOCALAPPDATA%\Programs\xcb\bin`, and `xcb upgrade` runs the same installer, which moves a running `xcb.exe` aside instead of overwriting it. Providers still need the Linux build in WSL2.
- On Linux, `xcb doctor --provider claude --qualify-sandbox` runs the sandbox test from the xcb binary and saves the result Claude needs, so a release install no longer needs a source checkout or Bun. The result now records Ubuntu's AppArmor user-namespace setting: when a reboot turns the restriction back on after a `sysctl` workaround, `xcb doctor` says the sandbox is no longer ready and explains the fix instead of reporting it ready. The documented fix is now xcb's AppArmor profile for `/usr/bin/bwrap` only, which survives reboots. Results saved by older versions must be taken again.
- Releases now include a Linux ARM64 build, `xcb-<version>-linux-aarch64.tar.gz`, built on an ARM64 runner with the same glibc 2.34 floor as the x86_64 build. CI builds it on every push to `main`.
- The npm release step no longer fails a publish that npm accepted. npm now
  reports the trusted-publisher configuration id without its `oidc:`
  prefix, and the release check rejected that form for 0.11.1 and 0.11.2.
  Both forms are accepted now.
- The npm release step waits up to 25 minutes for npm to serve a new
  version, polling more slowly as it waits, and logs what npm returned.
- Re-running the failed npm job after npm accepted the publish now
  finishes the release. The retry checks that npm serves the exact tarball
  from the run and does not publish again.
- The one-line installer and the installer it downloads now accept the same hosts: macOS on Apple silicon and Linux on x86_64 or ARM64. When the requested version has no build for your host, `curl -fsSL https://xcb.sh/install.sh | sh` says so before downloading anything, and a release install on an Intel Mac or another unsupported host stops with the same message instead of a failed download.
- `xcb upgrade` and `xcb update check` say when a release has no build for this host (for example `xcb 0.11.2 has no release build for linux-aarch64`) and point to the source install, instead of reporting a failed lookup or that no release exists.
- `xcb service install`, `status` and `uninstall` work on Linux: the supervisor runs from a systemd user unit in `~/.config/systemd/user`, logs to `~/.local/state/xcb`, and install says when `loginctl enable-linger` is needed to keep it running after logout. `xcb update enable` adds a daily systemd user timer. xcb never replaces or removes a unit it did not write.

## 0.11.2 - 2026-09-29

xcb keeps a separate model list for each account, so two accounts of the same
provider on different plans no longer share one list. xcb also supports Codex
CLI 0.158.0, and a model can now run its own checks with `workspace_exec`.

- Before, each refresh replaced the provider's single model list with what
  one account reported. A model only one Devin or Claude plan offers could be
  sent to another account, which then refused it and used up one of the
  task's attempts, and each account's refresh erased the others' lists. Now
  `xcb accounts refresh`, `xcb models refresh --account`, `xcb setup`, and
  every task start update only that account's list, and automatic routing
  sends a model only to an account that listed it.
- Model lists saved before this version still work: an account uses them
  until it reports its own list. `xcb models refresh` without `--account`
  still updates that shared list.
- `xcb models` names the accounts that can use a model when some account of
  that provider cannot. `xcb models --json` adds an `accounts` field with
  their ids to each row.
- The command runner setup `--refresh` no longer fails with
  `public-cache-ack ValueError` after the runner VM restarts. Acknowledging a
  published dependency cache now remounts it read-only and rehashes it, as
  prepare and recover already do.
- `workspace_exec` now accepts an absolute `cwd` inside the workspace and runs
  there. Before, a model that passed its absolute working directory got
  `invalid bounded offline command` on every call and could not run its own
  checks. Refusals now name the field that was wrong.
- Codex CLI 0.158.0 on macOS ARM64 passes the executable, tool, configuration,
  and sandbox checks. The previous supported builds, 0.157.1 and 0.156.1,
  remain supported.
- When Codex reports that the Flex processing tier or the service is
  temporarily out of capacity, xcb records a temporary provider failure. It no
  longer treats the account as failed for an unknown reason, and it does not
  count the failure against the account's usage limit.

## 0.11.1 - 2026-09-28

A Devin worker that starts with an empty model list no longer erases the
stored list.

- Before, one empty startup list deleted every stored Devin model and failed
  the task, leaving no route until the next `xcb accounts refresh`. Now the
  stored list is kept, and the task is sent again on another route. The
  prompt was never sent, so nothing is repeated, and the retry counts toward
  the task's usual limit of 4 attempts.

## 0.11.0 - 2026-09-28

xcb uses remaining subscription capacity and reset times when choosing a
route, and brings recent Claude and Codex conversations into your workspace.

- Automatic routing favors unused Claude and Codex quota approaching a reset,
  while considering overlapping usage windows and preserving task-quality
  requirements, explicit model choices, and known usage limits.
- `xcb sessions discover` finds Claude and Codex conversations active in the
  last 24 hours. `xcb sessions import --recent` copies their conversation
  context into xcb, preserves the original files, and avoids duplicate
  imports. Imported history starts no work until you send a new message.
- A first link with no `--relay` and no `XCB_RELAY_URL` stops with a message
  that names both. Before, it contacted a local development backend at
  `127.0.0.1:3210`, which could wait 30 seconds and report only "relay request
  timed out".
- `xcb link` prints the relay it asks for a sign-in code, and a relay timeout
  names the address that did not answer.
- A task whose chosen model has left the provider's current model list is
  sent again on another route, chosen from the refreshed list. Before, the
  task failed without starting. The prompt was never sent, so nothing is
  repeated, and the retry counts toward the task's usual limit of 4 attempts.

## 0.10.5 - 2026-09-27

xcb can recover a stopped Codex sign-in that used a different ChatGPT account
without replacing the account you already saved.

- After confirming the sign-in process has exited, `xcb recover` can release
  the held account. It discards credentials from a different ChatGPT identity
  and keeps the account's existing sign-in and authentication status.
- Recovery still refuses to proceed if the saved credentials changed while
  the sign-in was open.

## 0.10.4 - 2026-09-27

xcb renews an expired relay sign-in while keeping the same linked device and
its queued work.

- `xcb link --reauth` signs in again to the device's recorded relay. It keeps
  the device ID, encryption keys, and pending remote commands.
- Local coding tasks continue during renewal. The background relay finishes
  its current cycle before xcb replaces the saved sign-in.
- An interrupted renewal resumes from saved progress. xcb checks whether the
  server completed the change before retrying, and refuses to restore local
  state that was cleared or replaced.
- Renewal requires an upgraded background supervisor. An older supervisor
  must complete its normal upgrade before the sign-in can change.
- SDK discovery and compatibility pages now describe its published npm package.

## 0.10.3 - 2026-09-27

xcb supports Codex CLI 0.157.1, lets you reconnect an existing Codex account
from a newer sign-in, and shows relay errors alongside local supervisor errors.

- Codex CLI 0.157.1 on macOS ARM64 passes the executable, tool, configuration,
  and sandbox checks. The previous supported 0.156.1 build remains supported.
  xcb disables Codex's new background daemon and guardian context features.
- When a background service cannot find a provider on its PATH, it can keep
  using its saved executable only if that copy still passes xcb's checks.
  Upgrading xcb also rechecks builds the previous release did not support.
- `xcb accounts import-codex --account <account-id> --source <auth.json>`
  reconnects an existing account with newer credentials for the same identity.
  It preserves the account's enabled state and usage limits.
- Relay connection and publication failures remain visible when a local task
  also fails. Each warning clears after the relevant operation succeeds.
- Remote commands and the background supervisor coordinate cloud sign-in
  refreshes. They use the latest saved session and cannot restore a session
  that was cleared or overwrite one that was replaced during a refresh.

## 0.10.2 - 2026-09-27

xcb reconnects after a relay outage, recognizes accounts that need a fresh
sign-in, and lets Claude run on Linux after its sandbox checks pass. The
install and documentation pages are easier to use on a phone.

- A linked background supervisor stays running while it connects or waits
  to retry, so it can receive remote commands when the network recovers.
- Claude on Linux becomes available only after the existing sandbox checks
  pass for the installed bubblewrap build and current system settings.
- `xcb setup` prefers a working sign-in and asks you to reconnect an account
  whose credentials were rejected before reporting it ready.
- Exact-version upgrades find older releases directly. `--json upgrade`
  and `--json update install` return one JSON result and send installer
  diagnostics to stderr.
- Custom installation directories are quoted safely in shell startup files
  and copied PATH instructions, including directories with shell punctuation.
- Phone readers can open documentation navigation when they need it.
  Example copy buttons copy commands without their output, and install
  instructions link to the Linux sandbox prerequisites.

## 0.10.1 - 2026-09-27

Devin now finishes the tasks xcb routes to it, and xcb reports a turn that ends
without a reply instead of counting it as done. xcb also installs with one
command. This release carries every change from 0.10.0, whose release run
stopped before its final check.

- The release pipeline now waits up to 15 minutes for npm to serve a newly
  published version. The 0.10.0 package reached npm about five minutes after
  it was published, after the pipeline had stopped waiting.
- Devin: when Devin tries one of its own tools that xcb blocks, it now gets a
  one-time refusal and carries on with xcb's file tools instead of ending the
  turn. Devin also gets a short guide to xcb's tools and a tool list short
  enough to read in full. With the tested account, Devin completed a coding
  task on macOS with Devin CLI 3000.11.3.
- A turn that completes with no reply and no file changes now reports
  `failure: no_reply`. `xcb --json route` returns `provider_error`, `xcb run`
  exits 1 and names the next step, and a managed task waits for your reply
  instead of showing as finished.
- `curl -fsSL https://xcb.sh/install.sh | sh` installs the latest release.
  The Linux binary now runs on glibc 2.34 or newer (Ubuntu 22.04, Debian 12,
  RHEL 9, and later); 0.9.1 needed glibc 2.39.
- `xcb doctor` reports each account's health with one next step, and no
  longer says every check passed while an enabled account needs attention.
  `xcb accounts` columns line up.
- `xcb update enable` and `xcb update disable` work on Linux, and
  `xcb upgrade <version>` refuses to install an older release unless you add
  `--allow-downgrade`.
- In the terminal, one Esc or Ctrl-C no longer cancels a managed task: press
  it again within three seconds. Notices clear on time, and help, notices,
  and dialogs use plain words.
- Route reasons say how xcb classified the task, its capability tier, and
  the model's relative quality, cost, and speed. A public pricing promotion is
  named but never changes which route wins.
- The background supervisor records why it failed to start, keeps
  dispatching when one saved record can't be read, and shuts down within a
  deadline.
- The TypeScript SDK installs from npm: `npm install @hraness/xcb`.

## 0.10.0 - 2026-09-27

Devin now finishes the tasks xcb routes to it, and xcb reports a turn that ends
without a reply instead of counting it as done. xcb also installs with one
command.

- Devin: when Devin tries one of its own tools that xcb blocks, it now gets a
  one-time refusal and carries on with xcb's file tools instead of ending the
  turn. Devin also gets a short guide to xcb's tools and a tool list short
  enough to read in full. With the tested account, Devin completed a coding
  task on macOS with Devin CLI 3000.11.3.
- A turn that completes with no reply and no file changes now reports
  `failure: no_reply`. `xcb --json route` returns `provider_error`, `xcb run`
  exits 1 and names the next step, and a managed task waits for your reply
  instead of showing as finished.
- `curl -fsSL https://xcb.sh/install.sh | sh` installs the latest release.
  The Linux binary now runs on glibc 2.34 or newer (Ubuntu 22.04, Debian 12,
  RHEL 9, and later); 0.9.1 needed glibc 2.39.
- `xcb doctor` reports each account's health with one next step, and no
  longer says every check passed while an enabled account needs attention.
  `xcb accounts` columns line up.
- `xcb update enable` and `xcb update disable` work on Linux, and
  `xcb upgrade <version>` refuses to install an older release unless you add
  `--allow-downgrade`.
- In the terminal, one Esc or Ctrl-C no longer cancels a managed task: press
  it again within three seconds. Notices clear on time, and help, notices,
  and dialogs use plain words.
- Route reasons say how xcb classified the task, its capability tier, and
  the model's relative quality, cost, and speed. A public pricing promotion is
  named but never changes which route wins.
- The background supervisor records why it failed to start, keeps
  dispatching when one saved record can't be read, and shuts down within a
  deadline.
- The TypeScript SDK installs from npm: `npm install @hraness/xcb`.

## 0.9.1 - 2026-09-27

Every command's help fits on one screen and errors say what to run next,
for people at a terminal and for agents calling xcb.

- `xcb --help` is a short grouped list of the everyday commands, and
  `xcb help advanced` shows the full surface. Help text wraps within 100
  columns and drops colors under `NO_COLOR` or `TERM=dumb`.
- A usage error is one sentence naming what went wrong and the help to
  read next. With `--json`, or when the caller is an agent, the same error
  is a single JSON object on stdout.
- Plain `xcb` outside a terminal prints a short start screen and exits,
  instead of waiting for input; in a terminal it still opens your thread.
- When no provider is set up, xcb suggests `xcb setup <provider>`, and
  `xcb doctor` ends with a count of what it checked.

## 0.9.0 - 2026-09-26

Plain `xcb` opens one thread for all your projects from any directory, and xcb
picks each task's project directory and says why. Projects are directories, so
a grant for one directory never authorizes work in another.

- Plain `xcb` and `xcb chat` open your thread, one conversation per machine
  whose tasks can run in any project directory. Each prompt is bound to a
  directory when its task is created: a path or project name in the prompt,
  the focused project, a continuation of your last task, then the launch
  directory or recent work. The reply says which directory and why, and
  `xcb workspaces why <task>` shows the full record. xcb asks instead of
  guessing when a prompt names an unregistered directory or several projects,
  and saves nothing until you pick. `--cwd` is a hint for the thread.
  `xcb chat --new` still starts a new project view for the current directory.
- `/workspace` focuses the thread on a project, moves an unstarted task
  (`/workspace move`), starts a waiting task (`/workspace go`), clears the
  focus, or registers a directory (`/workspace add`). A less certain choice,
  or a prompt that names a project other than the focused one, waits 8 seconds
  before it starts so you can move it. Alt-Left and Alt-Right move the focus,
  task messages carry a project chip, and the overview shows one card per
  project.
- A project is now a directory. The backlog, grants, schedules, working
  memory, and Wordcell binding belong to the directory and are shared by the
  thread and every project view over it. A grant authorizes automatic work
  only in its own directory, and worker tools read only their own directory's
  backlog and memory. Commands that took a conversation ID take `<dir|name>`,
  and a project view's ID still works: `xcb projects configure <dir>`,
  `xcb memory configure|status|search <dir>`, `xcb backlog memory <dir>`, and
  `xcb backlog add|program`, `xcb schedules add|program`, and
  `xcb daemons run` (add `--workspace <dir>` to target the thread by ID).
  `xcb backlog --workspace <dir>` filters by directory. In the thread,
  `/project`, `/memory`, `/schedule`, and `/backlog add` never guess a
  directory.
- `xcb workspaces` lists, adds, hides, and explains the directories the thread
  picks from, and `xcb workspaces conflicts` lists what the upgrade changed.
  xcb refuses `/`, your home directory, its hidden directories, `~/Library`,
  xcb's state, and system directories as projects.
- Tasks in a directory and a directory inside it (`/repo` and `/repo/site`)
  now run one at a time.
- Upgrading moves grants and Wordcell bindings to directories. A grant that was
  the only grant for its directory keeps its budget and expiry and now covers
  every conversation over that directory, including remote dispatches and
  thread tasks bound there. When several conversations over one directory had
  active grants, the kept grant is paused as `paused by upgrade` until you
  resume it; budgets are never added together. Different Wordcell bindings
  for one directory unbind until `xcb memory configure <dir>`.
  `xcb doctor --upgrade-plan` previews all of this on a private copy. Quit
  every xcb terminal before installing.
- Remote dispatch lands in the device's thread (`conversation` is
  `c_global`), and the result adds `task`, `workspace`, and `workspaceSource`.
  The fleet projection adds `xcb` and `capabilities`, and its task rows add
  `workspaceSource`. A relative workspace is a project name on that device,
  `@infer` lets the device pick from the prompt without guessing, and an
  absolute path binds exactly. Home, its hidden directories, `~/Library`, `/`,
  xcb's state, and system directories are refused.
- `xcb conversations --json` rows add `isThread`; the thread's row has
  `"workspace": null`.
- `xcb memory status --json` now prints `{workspace, binding, conflicts}`
  instead of the binding object or `null`; the binding is under `binding`, and
  `conflicts` lists the directory's open upgrade conflicts. Binding rows from
  `xcb memory configure` and `memory status` name `workspace` instead of
  `conversation`.
- Relative directories and project names in every command start at `--cwd`,
  including `xcb projects`, `xcb memory`, `xcb backlog`, `xcb schedules`, and
  `xcb daemons run` and their `--workspace`.
- Managed schema v7 cannot be downgraded; restore the pre-v7 backup to roll
  back. The upgrade writes `managed/managed.pre-v7.<time>.sqlite` in the state
  root first when there is room.

## 0.8.14 - 2026-09-26

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
  Documents, Desktop or Downloads. A service installed by an earlier
  version keeps working; reinstall it to turn on the log.
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
