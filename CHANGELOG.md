# Changelog

Release notes for the `v<version>` tag channel. Published GitHub Release
assets, not this file, are the evidence that a version shipped; see
[docs/publishing.md](docs/publishing.md).

## 0.8.0

- A session grid above chat shows agent names, routed models, activity, and
  response previews with category labels and colors. It grows up to half the
  terminal height, with independent scrolling and a compact view on short screens.
- F6 browses the grid from the keyboard. With `/mouse` enabled, the wheel scrolls
  the panel under the pointer and clicking a card adds an agent reference to the
  draft. References preserve the current chat and task target and send nothing.
- `/overview project|all|hide|show` controls scope and visibility. Questions and
  approvals keep their existing controls above the overview.

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
