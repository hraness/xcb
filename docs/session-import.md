# Bring recent conversations into xcb

Find Claude and Codex conversations used in the last 24 hours:

```sh
xcb sessions discover
```

Copy their user and assistant messages into xcb:

```sh
xcb sessions import --recent
```

The result lists the imported conversation IDs. Open one with
`xcb chat --resume <conversation-id>` or choose it with `/sessions` inside
xcb. Send a new message to continue with xcb's normal account and model
selection. Importing history does not submit the old prompts or take control
of the original provider process.

Both commands accept `--hours` to change the activity window and `--provider
codex` or `--provider claude` to select one provider. To import only one of the
listed candidates, use `xcb sessions import <candidate-id>`. Add `--json`
before `sessions` for structured results.

Sessions started in your home directory need a project before xcb can open
them as a conversation. Select one candidate and name an existing project:

```sh
xcb sessions import <candidate-id> --workspace /path/to/project
```

`--workspace` applies only to an individual import and cannot be combined
with `--recent`. xcb records the original directory in the imported history.
The selected project must pass the usual directory checks; home and xcb's
private state are refused. An imported conversation keeps its chosen project
when you import it again. xcb does not guess a project for home-started sessions.

xcb reads Codex session files under `$CODEX_HOME/sessions`, or
`~/.codex/sessions`, and Claude project files under
`$CLAUDE_CONFIG_DIR/projects`, or `~/.claude/projects`. The source files stay
in place. Repeating an import adds newly recorded messages without duplicating
the history already copied.

The activity window finds recent work; it does not prove that a session is
still running. xcb skips subagent logs and does not read credential files or
provider settings. It imports user and assistant text, excluding tool results.

Each scan checks up to 32,768 directory entries and reads up to 128 MiB in
total, with at most 8 MiB read from one file. It reads conversation bodies
from up to 256 recent files. Codex subagents identified by their first record
are skipped before reading their bodies or using one of those file slots.
Large files use their beginning and most recent tail.
Each conversation retains up to 256 messages or 1 MiB of text per import.
The next task receives up to 64 KiB of imported context. Results identify
partial histories and skipped files. Older snapshots cannot append old messages
after a newer imported tail.
