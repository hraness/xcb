# Changelog

Release notes for the `v<version>` tag channel. Published GitHub Release
assets, not this file, are the evidence that a version shipped; see
[docs/publishing.md](docs/publishing.md).

## 0.6.0 (unreleased)

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
