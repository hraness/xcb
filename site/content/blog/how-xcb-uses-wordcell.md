A note explaining why a parser stops after three retries can matter as much as the code itself. If that decision lives in a separate Markdown folder, the next agent working on the parser needs a way to find it. xcb lets project workers search that folder through Wordcell and return the path of each matching note.

## Search the notes where you keep them

[Wordcell](https://wordcell.io) adds search and a link graph to a knowledge base kept as Markdown files. Its exact search reads the current files without a model download or network request. xcb uses that mode, so a corrected note appears in the next search.

Bind one vault and an installed Wordcell executable to a project, then check the connection:

```sh
xcb memory configure <dir> --vault /absolute/project/vault --wordcell /absolute/bin/wordcell
xcb memory status <dir>
xcb memory search <dir> "parser decision"
```

xcb records the executable and vault identity and checks them before each call. If the executable changes or the directory is replaced, bind it again. For a script launcher, xcb also records its interpreter. These checks cover the launcher and interpreter, so you still need to trust the installed Wordcell package.

## Let the worker choose when to search

A managed project worker gets the `xcb_memory_search` tool. It supplies a query and an optional result count:

```json
{ "query": "parser retries", "limit": 8 }
```

The worker receives matching notes with their paths. It cannot choose a different vault through that tool, and vault text enters its context only when it searches. Recent xcb task summaries are a separate source of working memory that fresh workers can receive automatically.

This integration uses exact matching across words, phrases, titles, tags, and paths. Wordcell's semantic search, graph expansion, and Git-history options are outside this path. The note remains historical context: the agent should check a past decision's applicability before using it to change current code.

## Save a decision from a finished task

To keep something a task learned, write the note you want to retain and save it against that task:

```sh
xcb memory promote <task-id> --body-file decision.md
```

xcb saves the supplied text and adds the source conversation and task. It does not automatically export the conversation or copy a task summary into the vault. Saving the same note for the same task again produces the same note identity, and Wordcell refuses to overwrite an existing file.

A successful save returns the expected note path and revision. An interrupted or unclear result reports uncertainty instead of success, so a script can distinguish a confirmed save from one that needs inspection.

The memory tool itself is read-only for workers. Keep the vault outside the project workspace if you also want to prevent ordinary workspace file tools from changing it. The [project memory reference](https://github.com/hraness/xcb/blob/main/docs/project-agents.md#working-memory-and-wordcell) covers the binding, search, and save commands.
