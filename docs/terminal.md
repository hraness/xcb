# Terminal workspace

Plain `xcb` (or `xcb chat`) opens your thread from any directory. xcb chooses
the project directory, account, and model for each task. `/sessions` (alias
`/resume`) lists the thread first and then `project view · <name>` rows.
`xcb chat --new` opens a new project view for the current directory.
`/rename <name>` changes a conversation's title. `/status` shows routing and
agent state.

Account, model, session, conversation, and pane pickers open on the current choice when it is available and mark it “current”. Empty lists explain how to populate them; if a filter has no matches, Ctrl-U clears the filter. An account that needs sign-in or is turned off shows the command to reconnect or enable it.

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
| Move the thread's focus between projects, or switch conversations in a project view, with an empty prompt | Alt-Left / Alt-Right |

Ctrl-C closes a dialog, clears a nonempty draft, or requests cancellation when
the prompt is empty and work is active. With no work or draft, it quits.
Ctrl-D deletes the next character; it quits only with an empty prompt.
Escape closes the current panel, returns a scrolled transcript to the latest
output, or requests cancellation. Task cancellation remains pending until the
worker has stopped and xcb has recorded its result.

Paste preserves multiple lines without sending them. Drafts accept up to
256 KiB; a larger paste is refused as a whole. `/attach <path>` adds an image;
`/detach [number|all]` removes it. With no number, `/detach` removes the last
attachment. Mouse capture starts on for wheel scrolling; `/mouse`
turns it off for terminal text selection.

## Work in the thread

Each prompt in the thread becomes a task in one project directory. The reply
names the directory and why xcb chose it, for example
“Started Fix the parser in `app` · named `app` · /workspace to move”. The
[managed harness guide](managed-harness.md#choosing-a-tasks-directory) lists
the rules in order.

The header shows `xcb · all projects · 2 active`, or `xcb · → app` while a
project is focused, and the footer counts known projects. Task messages in the
transcript carry a dim chip with their project's name. Task details show a line
such as ``Project: app · continuing in `app` (medium)``, and `Moved from <task>`
for a moved task.

| Command | Effect |
| --- | --- |
| `/workspace` | Pick a project to focus. |
| `/workspace <name\|path>` | Focus that project. If your last prompt's task has not started, it also moves that task there. |
| `/workspace move <task> <name\|path>` | Move an unstarted task to another project. |
| `/workspace go [task]` | Start a waiting task now. |
| `/workspace clear` or `/workspace all` | Clear the focus. |
| `/workspace add <dir>` | Register a directory as a project. |

The focus belongs to this terminal. While a project is focused, new prompts,
`/backlog`, and `/project` use it unless the prompt names another path or
project. Opening a thread card in the overview focuses its project, and
Alt-Left and Alt-Right move the focus along the overview's projects, wrapping
through “all projects”. A directory that holds other projects cannot be
focused until you register it with `/workspace add`.

When xcb cannot tell which project a prompt is for, it keeps your draft, saves
nothing, and opens a picker of known projects. Entries marked “new” are
directories named in the prompt that xcb has not seen; picking one registers it.
Picking a project focuses it and sends the draft again. Escape closes the picker
and keeps the draft.

A less certain choice, or a prompt that names a project other than the focused
one, waits 8 seconds before it starts and shows a chip such as
`→ app · starts in 6s · /workspace go`. Only prompts typed in the thread wait.
A started task cannot move; cancel it and send the prompt again.

`/new` in the thread clears the focus and the guidance target and creates no
conversation; in a project view it starts a new view. `/sessions` offers
`＋ new project view · <name>` when the focus, launch directory, or open view
names a directory.

`/project`, `/memory`, `/schedule`, and `/backlog add` act on a project only
when you have made it clear: a project you name (for `/project` and `/memory`),
the project view's directory, the focus, or the selected task's directory.
Otherwise they ask and save nothing. `/project grant [dir|name] <tasks> <hours>
<goal>` reads a first word that is a whole number as `<tasks>`; write a
directory named only with digits as a path, such as `./2026`.

## Edit with Vim keys

`/vim` switches the composer to modal editing; `/vim` again turns it off.
Insert mode keeps every key above. Esc enters Normal mode,
and the prompt gutter shows `I` or `N` in place of the `›` marker. Enter still
sends from either mode, and a send returns the composer to Insert mode. `/`
in Normal mode reopens the command menu.

| Normal mode | Keys |
| --- | --- |
| Move | `h j k l`, `0 ^ $`, `w b e` and `W B E`, `gg G`, `{ }`, arrows, Home, End |
| Find on the line | `f F t T` with a character; `;` and `,` repeat |
| Operate on a motion | `d`, `c`, `y`; doubled (`dd cc yy`) for whole lines |
| Whole-line shortcuts | `D C` to the line end, `Y` for the line, `J` joins lines |
| Small edits | `x X` delete, `s S` substitute, `r` replaces one character |
| Paste | `p P`; whole lines when the deletion or yank was linewise |
| Undo and redo | `u` and Ctrl-R |
| Return to Insert | `i a I A`, or `o O` on a new line |

Counts work before a motion or an operator (`3w`, `d2j`, `5x`). Esc in Normal
mode drops a half-typed command; with nothing pending it keeps its usual
meaning and interrupts running work. Ctrl-C, Ctrl-G, Ctrl-V, and the panel
keys keep their meanings in both modes, and Ctrl-D still quits only with an
empty prompt. Vim editing covers the draft only; it does not add `:` commands,
visual mode, registers, or text objects.

## Keep an eye on sessions

The overview above chat shows your sessions. Each card shows
the session name, routed model, activity, and a preview of its latest response.
Labels accompany the colors for questions, approvals, completed work, usage
limits, and problems. A session that failed without a response shows its
failure reason in place of the response. A model is shown after routing; thinking is shown only
when the provider reports it.

The overview starts with sessions active or updated in the last day, plus the
conversation you have open. Use `/ovw all` to include older sessions. Cards use
up to three fifths of the terminal height; a tall terminal can show more rows.
The heading shows how many cards are visible and how to reach the next page.
Press F6, then use arrows or PageUp/PageDown to browse. You can also type
`/ovw next` or `/ovw prev`, or scroll the mouse wheel over the cards. Scrolling
below the cards scrolls the transcript. `/mouse` toggles mouse capture so you
can use the terminal's own text selection.

Each card has a colored number on its bottom border. The number and color
stay with that session while you filter, sort, or page through this terminal.
Running borders gently brighten and dim; reduced-motion mode keeps them steady.
Use `/pick 3` to select pane three. Enter or a click adds a session reference
to your draft; a project card in the main thread focuses that project.
Adding a reference sends nothing. Escape returns to chat.

Type `remove session 3` or `/hide 3` to hide that card in this terminal.
`show sessions` or `/show` restores hidden cards. These commands keep session
history and leave running work intact. Hiding lasts until this terminal closes.

In the grid, `1` shows all sessions, `2` active work, and `3` attention.
`/` or Ctrl-F filters by name, model, status, or ID. Enter finishes filtering;
Escape clears the filter or returns to chat. Your draft stays intact.
The overview holds up to 128 sessions and 2,048 bytes per response preview.
Short terminals use a compact strip to leave room for typing.

Common commands have short aliases: `/attn` (attention), `/ovw` (overview),
`/hist` (history), `/proj` (projects), `/work` (workspace), `/stat` (status),
`/guid` (steer), `/stop` (cancel), `/ans` (reply), and `/schd` (schedules).
The full names and existing single-letter shortcuts still work. Type `/` to
browse commands and see their aliases.

The composer wraps long drafts to fit the window. Arrow keys follow the visible
rows; wrapping preserves the original text. Use Shift-Enter or Alt-Enter,
depending on your terminal, to insert a newline.

## Guide an agent

Open `/agents`, `/backlog`, or `/attention`, select a task, and press `s` in its
details to guide it. The prompt displays the selected task ID. Enter sends
guidance for that task's next allowed turn. Tab queues a separate task in the
current conversation. `/task` returns the composer to new work.

Press `a` in task details to answer its current question. The answer carries
the question's revision, so a replacement question cannot receive an old reply.
Press `x` to request cancellation of that exact task. `/cancel` uses the selected
target, or asks which task when several could be cancelled and none is selected;
in the thread each choice shows its project.
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
context rewind, and native terminal scrollback are separate features; the
current xcb transcript remains inside its terminal workspace.
