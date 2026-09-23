# Changelog

Release notes for the `v<version>` tag channel. Published GitHub Release
assets, not this file, are the evidence that a version shipped; see
[docs/publishing.md](docs/publishing.md).

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
