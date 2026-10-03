# Codex-familiar terminal workspace

## Outcome

Make the daily xcb interaction feel familiar to a Codex CLI user: a quiet
transcript, capable prompt editor, predictable keys, searchable context, and
clear control of ongoing agents. Preserve automatic routing and the existing
project/task authority model. Study the current OSS implementation and an
isolated real CLI session; do not infer behavior from remembered shortcuts.

Reference source: [OpenAI Codex](https://github.com/openai/codex/tree/e7165c2dbb14c912d682cef254d7430a5137885a),
installed CLI `0.156.1`. Reference code is inspected for behavior; xcb keeps its
own implementation and identity. Record terminal-dependent differences.

## Delivery graph

1. Read-only parallel audits: xcb TUI, runtime control boundaries, Codex OSS.
   Root performs actual isolated Codex terminal interaction and source integration.
2. Freeze interaction contracts, then parallel ownership in this checkout:
   - Composer lane: `composer.rs`, new editor/recovery modules and their tests.
   - Presentation lane: `render.rs`, new Markdown module and render tests.
   - Harness lane: core UI contract and runtime targeted actions/history/rename.
   - Root: App integration/key dispatch, CLI wiring, docs, versions and acceptance.
3. Join: behavior tests plus isolated PTY interaction, narrow/wide terminal
   layouts, Unicode/paste, concurrent state changes and rejected operations.
4. Independent review and repairs; required aggregate native, compatibility and
   site checks on the converged source tree.
5. Protected PR/CI, immutable release, actual artifact admission, installation,
   publication and production verification.

Workers share the checkout and must preserve one another's edits. Shared
interfaces and manifests have one owner; focused evidence has one owner per
lane. Root owns the final integration and delivery gates.

## Interaction contract

- Standard editing keys keep their editing meaning: Ctrl-A/E/B/F/P/N/U/K/W/Y,
  Alt-B/F/D/Backspace and Unicode-safe cursor/word operations. Enter submits;
  Shift/Alt-Enter and Ctrl-J insert newlines. Bracketed paste is never sent as
  keystrokes or silently clipped.
- Ctrl-R searches complete prompt history; cursor movement and history recall
  preserve the current draft. Ctrl-G opens the configured external editor with
  terminal restoration and bounded private temporary content.
- Ctrl-T opens conversation history, F3 searches it, Ctrl-O copies the last
  assistant answer, Ctrl-L clears the visible transcript without deleting data.
  `/help` and `?` expose current keys; help and pickers work in short terminals.
- Managed task guidance has an explicit displayed target. Enter can steer that
  selected task at its next authorized turn; Tab queues ordinary new work.
  Neither action answers an approval or expands a project grant. Ambiguous or
  stale targets never silently select another task.
- Attention/task/backlog views update live while preserving selection and query.
  Answer and cancel actions carry task identity and observed revision. A stale
  answer cannot be applied to a replacement question.
- Prompt recovery is private, bounded and isolated across concurrent terminals.
  Recovered drafts are never sent automatically and never overwrite newer input.
- Session rename and older transcript access preserve identity, worker custody,
  approvals and history. Search and page limits are visible and honest.
- Quiet startup information, `›` prompt, restrained status/footer hints, readable
  Markdown/code/diffs, and bottom-oriented selection panels replace visual noise.
  Keep xcb's provider-neutral route and attention information accessible.

## Acceptance

Demonstrate familiar editing, multiline paste, full-body history search, editor
round-trip/failure recovery, contextual Ctrl-C/Esc, live attention, exact task
cancellation/reply races, queued/steered input rejection recovery, transcript
navigation/search/copy, rename persistence and small-screen usability. Preserve
all existing program/inbox/grant/custody acceptance. No user provider, schedule
or service activation is needed to validate or install the release.

## Progress

- [x] Parallel source audits and current xcb main integration.
- [x] Actual installed Codex TUI interaction and comparison record.
- [x] Composer, presentation and harness lanes implemented and joined.
- [x] Independent review and reported repairs.

Final gate receipts and terminal captures are retained in the local validation
directory below. The immutable release tag and `site/published-release.json`
record publication after the actual artifacts have passed verification;
installation and production checks are recorded with the delivery evidence.

## Review record

Codex (AI agent) authored the implementation. Parallel AI reviews covered task
identity, stale questions, navigation races, terminal rendering, and private
input recovery. Repairs bind sends and project controls to the displayed
conversation, save pending operations before dispatch, preserve uncertain sends
across context switches, and expose inactive crash files for explicit recovery.
The integration owner keeps local terminal captures and check receipts in
`xcb-codex-ux-validation-20260923`; final release evidence is recorded after
artifact, installation, and production checks finish.
