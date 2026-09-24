# Terminal workspace

`xcb chat` opens a managed conversation. xcb chooses the account and model for
each task. `/resume` lists saved conversations; `/new` starts another one.
`/rename <name>` changes its title. `/status` shows routing and agent state.

The editor follows familiar Codex CLI keys. Type `/` to search commands, use
Up/Down to select, Tab to complete, and Enter to run. Escape closes a menu and
keeps your draft. Type `?` on an empty prompt or press F1 for scrollable help.

| Action | Key |
| --- | --- |
| Send, or guide the explicitly selected task | Enter |
| Queue new work in the current managed conversation | Tab |
| New line | Shift-Enter when supported; Alt-Enter or Ctrl-J elsewhere |
| Line start / end | Ctrl-A / Ctrl-E |
| Character left / right | Ctrl-B / Ctrl-F |
| Word left / right | Alt-B / Alt-F |
| Up / down; prompt history at an unchanged history entry or empty draft | Ctrl-P / Ctrl-N, Up / Down |
| Delete to line start / end | Ctrl-U / Ctrl-K |
| Delete previous / next word | Ctrl-W or Alt-Backspace / Alt-D |
| Paste the last deletion | Ctrl-Y |
| Search full prompt history | Ctrl-R; Ctrl-R goes older, Ctrl-S goes newer |
| Edit the draft with `VISUAL`, then `EDITOR` | Ctrl-G |
| Browse saved transcript | Ctrl-T |
| Find text in the transcript | F3 |
| Copy the last assistant answer | Ctrl-O |
| Clear the display, keeping saved history | Ctrl-L |
| Expand tool output | F4 |
| Recall the most recently updated queued task if it has not started and supports recall | Alt-Up |
| Open attention across agents | F2 or Alt-Down |
| Focus the agent overview; Enter adds a reference, Escape returns to chat | F6 |
| Switch conversations with an empty prompt | Alt-Left / Alt-Right |

Ctrl-C closes a dialog, clears a nonempty draft, or requests cancellation when
the prompt is empty and work is active. With no work or draft, it quits.
Ctrl-D deletes the next character; it quits only with an empty prompt.
Escape closes the current panel, returns a scrolled transcript to the latest
output, or requests cancellation. Task cancellation remains pending until the
worker has stopped and xcb has recorded its result.

Paste preserves multiple lines without sending them. Drafts accept up to
256 KiB; a larger paste is refused as a whole. `/attach <path>` adds an image;
`/detach [number|all]` removes it. With no number, `/detach` removes the last
attachment. Mouse capture starts off so terminal selection works; `/mouse`
enables wheel scrolling.

## Keep an eye on agents

The overview above chat shows sessions in the current project. Each card shows
the session name, routed model, activity, and a preview of its latest response.
Labels accompany the colors for questions, approvals, completed work, usage
limits, and problems. A model is shown after routing; thinking is shown only
when the provider reports it.

The grid uses at most half the terminal height and scrolls when more sessions
are present. Press F6, then use arrows or PageUp/PageDown to browse. Enter adds
the selected agent's reference and a response snapshot to your draft. Escape
returns to chat. With `/mouse` enabled, scroll over the grid to browse agents,
scroll below it to browse the transcript, or click a card to add its reference.

References identify the conversation or session and its observed task. Adding
one sends nothing and keeps your current chat and guidance target. Edit the
draft to describe what you want to do with that context, then send it from the
main chat. Existing task, project, and approval controls still apply.

Use `/overview all` to see other projects, `/overview project` to return to the
current project, or `/overview hide` and `/overview show` to control visibility.
The overview holds up to 128 sessions and 2,048 bytes per response preview.
Short terminals use a compact strip to leave room for typing.

## Guide an agent

Open `/agents`, `/backlog`, or `/attention`, select a task, and press `s` in its
details to guide it. The prompt displays the selected task ID. Enter sends
guidance for that task's next allowed turn. Tab queues a separate task in the
current conversation. `/task` returns the composer to new work.

Press `a` in task details to answer its current question. The answer carries
the question's revision, so a replacement question cannot receive an old reply.
Press `x` to request cancellation of that exact task. `/cancel` uses the selected
target, or asks which task when several could be cancelled and none is selected.
Approvals, account access, and uncertain
results keep their existing separate controls.

Lists and task details update while open. Selection follows the task ID as rows
change. A disappeared target cannot silently redirect an action to another task.
`/inbox` shows when saved guidance reaches a worker.

## Find earlier work

Ctrl-R searches complete prompt bodies, including later lines. Escape returns
the original draft. Enter recalls the selected prompt for editing.

Ctrl-T opens saved messages. F3 searches the loaded pages; Enter or Ctrl-N goes
to the next match, Shift-Enter or Ctrl-P to the previous one. Press `p`, or
PageUp at the top, to load older messages. The viewer retains up to 2,048
messages and 16 MiB of text. For longer histories, use the CLI's explicit page
cursor:

```sh
xcb history <conversation-id> --json --limit 128
xcb history <conversation-id> --json --before <first_sequence>
xcb history <session-id> --direct --json
xcb rename <conversation-id> "Parser maintenance" --expected-title "Old title"
xcb tasks cancel <task-id> --revision <observed-revision>
```

## Recover input

Each terminal keeps a private input journal under the native state directory's
`input-recovery/`. `/drafts` lists earlier inactive terminals and retained
rejected input. Recovery copies input into an empty composer in its original
conversation. It never sends it. A request without a confirmed send result is
marked uncertain; inspect the task or transcript before sending it again.

Journals preserve image references, context drafts, pending requests, and up to
200 prompts or 1 MiB of prompt history. Saving is checked every 750 ms and at
normal exit. A crash can lose edits made since the last save. Concurrent
terminals use separate files. Storage is limited to 128 terminal journals and
128 MiB; a save error is shown without deleting earlier input. In `/drafts`,
Ctrl-D opens a confirmation to discard a selected inactive journal or retained
draft. Live terminals cannot be discarded.

xcb implements these interactions itself. Provider-specific Codex commands,
context rewind, Vim mode, and native terminal scrollback are separate features;
the current xcb transcript remains inside its terminal workspace.
