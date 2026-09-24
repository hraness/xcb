# Agent overview above chat

## Outcome

Keep a compact view of project agents above the transcript while the main chat
remains the place to give instructions. Each card shows a persistent conversation
or direct session, its name, routed model, actual activity, and latest response.
Response labels and colors distinguish completed work, questions, approvals,
limits, failures, and uncertain results without inferring state from prose.

The grid grows with its content up to half the terminal height. It scrolls
independently and leaves space for the transcript and prompt. Narrow terminals
use fewer columns; short terminals use a compact strip. Dialogs and approvals
stay above the overview.

## Interaction

- Show current-project sessions by default; `/overview all` expands scope.
  `/overview project`, `/overview hide`, and `/overview show` control the view.
- F6 focuses the grid. Arrows, Home/End, and PageUp/PageDown browse it. Enter
  inserts a reference into the draft and returns to chat; Escape returns without
  changing the draft or cancelling work.
- With `/mouse` enabled, wheel events scroll the panel under the pointer. A
  card click inserts the same reference. Terminal text selection keeps its
  existing default because mouse capture remains opt-in.
- References include the stable conversation/session identity and observed task
  identity, with a short response snapshot. Inserting one sends nothing, switches
  no session, and changes no selected guidance/answer target. References are
  context in the prompt, not new authority to message another project.
- Preserve selection by identity across live updates. Never apply a stale hit
  rectangle to a replacement row. Reflow and clamp scrolling after resize.

## Implementation ownership

All lanes share `/Users/benguo/Documents/xcb-persistent-routing` and preserve one
another's edits.

1. `/root/agent_reference_explore`: core `AgentRow` contract and runtime
   projections, bounded latest-response reads, generic activity phases, tests.
2. `/root/agent_grid_explore`: `agent_grid.rs` and `render.rs`, responsive
   rendering, input handling, independent scrolling, rendering and event tests.
3. `/root/grid_acceptance`: isolated fixture and PTY acceptance, no providers.
4. Root: App dispatch and repaint integration, documentation, versions, final
   checks, independent review and delivery.

The shared contract is `View.agents: Vec<AgentRow>`, with `context`, optional
`task`, `title`, `workspace`, optional `model`, `state`, `activity`, `response`,
optional response `category`, and `updated_at_ms`. At most 128 rows enter the
view, with response previews capped at 2,048 bytes. Responses and model labels
must belong to the selected task; queued work must not inherit an older task's
response. Thinking status comes from event kinds without saving reasoning text.

## Validation and delivery

Join the runtime and terminal lanes before focused tests. Cover tiny and wide
layouts, Unicode, control characters, independent scrolling, mouse hit identity,
modal precedence, unchanged drafts/targets, response provenance, and changed
background agent repainting. Run the synthetic fixture in a real PTY. Independent
AI review follows the integrated diff, with fixes and affected checks repeated.
Run the repository's native, compatibility, and site gates on the converged tree.
Deliver through protected PR checks and the immutable release pipeline, verify
the released bytes, install, and publish the verified release record.
