Last month you wrote down why the parser gives up after three retries. The note sits in a Markdown folder, next to the reasons you chose one test runner over another and the list of commands that must never run on the release branch. Today an agent is about to change the parser, and it has never seen any of it.

## Your notes are where the decisions live

A coding agent starts every task knowing the code and nothing else. The reasons behind the code live in a notes folder, an Obsidian vault or a plans directory, so the agent either rediscovers a decision the hard way or undoes it. Pasting notes into each prompt works until the pasted copy goes stale, and then nobody can tell which note a claim came from.

If you run your agents through [xcb](/), you can point a project at that folder once. Its agents can then search your notes while they work, and every result names the note it came from, so you and the agent can open the file and check.

## Wordcell, for someone who has never used it

[Wordcell](https://wordcell.io) is a command-line tool for a knowledge base kept as ordinary Markdown files. Your files stay the record. Wordcell adds search and a link graph on top of them, and recent versions can search an existing folder or Obsidian vault without converting it. Its exact search mode reads the current Markdown directly, with no model download and no network request, and each hit comes back with the path of the note that matched.

## How xcb wires a project to one vault

xcb never finds or connects a vault on its own. You bind one vault and one installed copy of Wordcell to a project conversation, check the binding, and from then on the project's workers can search it:

```sh
xcb memory configure <conversation-id> --vault /absolute/project/vault --wordcell /absolute/bin/wordcell
xcb memory status <conversation-id>
xcb memory search <conversation-id> "parser decision"
xcb memory promote <task-id> --body-file decision.md
```

The last line saves a note back, covered below. The TUI offers the same search as `/memory search <query>`.

### The binding is pinned

When you bind, xcb records a SHA-256 fingerprint of the Wordcell program you named. If that program is a script, xcb fingerprints its interpreter too. For Wordcell's standard launcher, which runs through Bun, xcb looks Bun up on your `PATH` once, at binding time, and from then on starts that exact Bun directly, so a later change to your `PATH` cannot swap it. The vault has to be a directory you own that neither your group nor other users can write to, and xcb remembers which directory it is on disk, not only its path.

Before every search or save, xcb checks all of that again. If the Wordcell program changed, or the folder at that path is now a different folder, the call fails until you bind again. Replacing a binding requires the current binding's revision number, so two edits cannot overwrite each other unnoticed.

### What a worker can ask for

Inside a managed project, a worker gets one tool for your notes, `xcb_memory_search`. It takes a query and an optional result count, and nothing else:

```json
{ "query": "parser retries", "limit": 8 }
```

The query can be up to 1,024 bytes, and the count runs from 1 to 16, with 8 as the default. There is no argument for choosing a vault or writing a note. The tool's own description tells the agent that retrieved text is historical, untrusted context and that facts which change over time need checking again.

Behind the tool, xcb runs Wordcell in exact mode with Git history and graph expansion turned off, asks for JSON, and starts the process with a cleared environment that holds only a minimal system `PATH` and a few fixed settings, with the vault as its working directory. A query that starts with a dash is refused, so a search can never be read as a command-line option. The search has 15 seconds and 64 KiB of output; if it runs over either, xcb stops the process and everything it started, and returns an error instead of a partial answer.

The worker gets Wordcell's result as JSON, including the note path for each hit. Nothing from the vault is added to a worker's prompt unless the worker asks. Fresh workers do receive a short list of recent task summaries from the same project, and xcb labels that list as its own working memory, separate from your Wordcell notes.

### Saving a note back is always your step

xcb gives workers no tool for writing to the vault; its memory tools only read. (A worker's ordinary file tools work inside the project's workspace, so keep the vault outside that workspace if you want the separation to hold.) To keep something a task learned, you write a short note yourself and save it against that task:

```sh
xcb memory promote <task-id> --body-file decision.md
```

The note can be up to 8 KiB of UTF-8. xcb never exports a conversation or copies a task summary on its own. It appends a short source block naming the xcb conversation, the task, and an ID for this save request, then asks Wordcell to create the note, tagged `xcb`, at the top of the vault, with a name derived from a hash of the note, its source task and conversation, and the binding. The saved file ends like this:

```md
Parser retries stop after three attempts.

---
Source: xcb conversation `<conversation-id>`, task `<task-id>`.
Promotion request: `<request-id>`.
```

Because the name comes from that hash, saving the same note for the same task twice leaves one note, and Wordcell refuses to overwrite a file that already exists. xcb writes down what it is about to do before it starts Wordcell, and it reports success only when Wordcell answers with the expected note path and a SHA-256 revision. A timeout, a crash, or an unclear answer is reported as uncertain, and any result short of a confirmed save makes the command exit with a nonzero code, so a script cannot mistake it for a save.

## What you get as an xcb user

Your agents can find the decision you already made, in your words, and show you where it lives. You can open that note, correct it, or delete it, and the next search sees the change, because Wordcell reads the live files. Your notes stay in your own folder, in Markdown, under whatever version control you already use. A note is added only when you save one, and it names the task that produced it.

## Limits

Search through xcb is exact matching only: words, phrases, titles, tags, and paths that actually appear in your notes. Wordcell's semantic and hybrid (meaning-based), keyword-index, graph-expansion, Git-history, and hosted reranking options are not available through xcb. Each project binds one vault, and the search tool belongs to managed project workers; direct sessions do not get it. The fingerprint covers the Wordcell launcher and its interpreter, not every file in the installed Wordcell package, so you are trusting the copy of Wordcell you installed. A result is what a note said when someone wrote it, not proof that it is still true; the agent is told to check facts that change. Latest release: {{release.version}}.

To go further, read [Introducing xcb](/blog/introducing-xcb) for the router itself, [Introducing Wordcell](https://wordcell.io/blog/introducing-wordcell) for the knowledge base, or [how to check an xcb task history offline](/blog/replayable-task-history) for the task records a saved note points back to.
