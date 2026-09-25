An agent that finishes a task hands back its own account of the work: a summary, a list of changed files, a line saying the tests passed. xcb also keeps a chain of local entries for every managed task, one for each change of state, and `xcb tasks verify <task-id>` replays that chain with no network. If an entry was changed after it was written, or a step in the middle is gone, the command fails and says where.

## A log written by the thing it describes

Software gets written quickly now. A model produces something that looks finished, survives the demo, and falls over the second time someone uses it. That is vibe-coded slop, and the common response is more agents: one to write, one to review, a scheduler to keep them working overnight. More agents means more work happens while nobody is watching, and the morning question becomes what they did.

The usual answer is a log. The same program writes it, can rewrite it, and is also the one telling you everything went fine. A crash that leaves a half-written row, a bug that skips a step, and a hand edit that tidies up an awkward entry all look the same to a reader. A task list that says "done" with no way to check how it got there is a fragile foundation for anything built on top of it.

## When a history can be checked by anyone

Suppose instead that each step writes down exactly what the task looked like at that moment, and each step points to the one before it. Later, a checker that knows nothing about the agent, the provider, or your accounts walks back from the last step to the first and redoes the bookkeeping. If everything lines up, the history on disk is the one that was written step by step. If one entry was edited or dropped, the walk stops there.

That check covers the record and says nothing about the quality of the work. Once you know the record is intact, your review time can go to whether the work is right.

## How xcb records each change to a task

xcb stores each managed task as a record with a revision number. Every change of state, such as pausing for your answer or accepting your reply, produces a new revision. Before a revision is saved, xcb passes it through a one-step program on the ALGAL runtime whose only job is to accept the record as given. ALGAL produces an entry holding the input, the output, a fingerprint of that program, and a fingerprint of the entry itself, and xcb stores that entry next to the task.

A fingerprint here is a SHA-256 digest of a canonical encoding of the data, so changing any field changes it. The stored task keeps the fingerprint of its latest entry. The record inside each entry carries the fingerprint of the entry before it, and the first revision points at a fixed placeholder, `sha256:pending`, instead.

```text
stored task   latest = fp(entry 3)
entry 3       revision 3, previous = fp(entry 2)
entry 2       revision 2, previous = fp(entry 1)
entry 1       revision 1, previous = "sha256:pending"
```

## The rules the check enforces

Starting from the stored task, the verifier walks backward and requires four things at every step.

1. **Replay reproduces the entry.** ALGAL reruns the entry against an empty store with no host tools, and the result must match what was written. Nothing in this step calls a provider or the network.
2. **The entry matches the task.** The record inside the entry must equal the task at that revision, field for field, apart from the pointer to the entry itself.
3. **No gaps.** Each earlier entry has a revision exactly one lower and the same task identity, and the walk must end at revision 1, whose predecessor is the placeholder.
4. **The same rules made every entry.** Each entry's program fingerprint must match the one stored on the task, and replay uses the transition program compiled into your xcb build.

In outline, written for this post rather than copied from the source:

```python
def verify(task):
    expected, replayed = task, 0
    while True:
        entry = find_entry(task.id, expected.revision, expected.latest)
        if entry is None:
            fail("a revision is missing")
        if not replay_matches(entry) or entry.program != task.program:
            fail("replay does not match")
        if entry.record != expected:          # ignoring the self-pointer
            fail("entry does not match the stored task")
        replayed += 1
        if expected.revision == 1:
            if entry.record.previous != "sha256:pending":
                fail("history does not start at the beginning")
            return {"verified": True, "revisions": replayed}
        expected = previous_record(entry)     # revision - 1, same identity
```

Running it prints one JSON object:

```sh
xcb tasks verify <task-id>
```

```json
{"taskId": "<task-id>", "verified": true, "revisions": 3,
 "receipt": "sha256:...", "policy": "sha256:..."}
```

`revisions` counts the entries replayed. `receipt` is the fingerprint of the latest entry and `policy` is the fingerprint of the transition program. Any failure exits with an error instead of this object.

## What the tests hold it to

xcb's test suite pins the behavior with real task histories. In one test, a task is created, pauses for input, and gets an answer, and the verifier replays three revisions. The test then rewrites the stored task's goal to "different task" directly in the database, and verification fails. Another test rolls a state database back to an older schema, reopens it so xcb upgrades it, and checks that the existing task still verifies.

Scheduled and backlog programs get the same treatment at a different point. xcb pins a planner program's content and inputs by fingerprint and re-checks both before each step. A saved checkpoint that was changed cannot resume, even when whoever changed it recomputes the checkpoint's own fingerprint, because the checkpoint must still agree with the pinned program. The tests try this with changed inputs, a swapped executor fingerprint, a changed request fingerprint, and an edited step record.

Automatic continuation reads the same task state. A contract test holds that automatic continuation never fires for a turn whose outcome xcb could not confirm, a cancelled turn, a completed turn, or one waiting for your attention.

## Checking a copy on another machine

Every xcb command accepts `--state <dir>` to use a state directory other than the default `~/.local/share/xcb`, so you can copy a state directory to a machine you trust and run the check there:

```sh
xcb --state /path/to/copied-state tasks verify <task-id>
```

Opening a state directory applies xcb's normal schema upgrades and, at most once a day, its 30-day retention pass, so point it at a copy you can afford to change, not the only one.

## What a passing check does not tell you

A verified history says the entries on disk are complete and unchanged since xcb wrote them, one step at a time. It does not say the agent's work is right. The check does not read the code, rerun the tests, or ask the provider what happened, and xcb's README states the same limit: the check does not vouch for provider claims or real-world outcomes.

The check compares entries with each other and with the stored task. Nothing is signed, and the entries live in your local state directory, so someone with write access who rebuilds every entry and every fingerprint consistently would produce a history that passes. What it catches is an edited entry, a missing step, or a half-written row.

The verifier refuses a history longer than 1,024 revisions. Finished tasks older than the 30-day retention window can be deleted together with their history (tasks that other live work still refers to are kept), so check a task before then if you want to keep the result. The managed harness that records these histories is experimental.
