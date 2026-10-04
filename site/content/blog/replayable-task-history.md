A task marked complete tells you where an agent stopped. Its history can tell you how it got there: when it started, when it asked a question, and what happened after you replied. To inspect that sequence, xcb can replay the task's local records without contacting a provider.

## Link each step to the one before it

A chain of records gives a checker something more specific to test than a list of status messages. Each record contains a revision number and a fingerprint of the previous record. Changing one record without updating the rest breaks that link; removing a step leaves a gap.

The fingerprint is a SHA-256 digest of a canonical encoding, so the same data produces the same fingerprint. Starting at the latest record, a checker follows the links backward and compares the revisions and task identity at each step.

```text
stored task   latest = fingerprint(entry 3)
entry 3       revision 3, previous = fingerprint(entry 2)
entry 2       revision 2, previous = fingerprint(entry 1)
entry 1       revision 1, previous = "sha256:pending"
```

This checks whether the saved records agree with one another. It does not establish their authenticity: someone who can rewrite the whole chain and every fingerprint can construct a different history that also passes. A trusted signature or a separately retained copy would answer a different question.

## Replay the records xcb saved

xcb stores each managed task with a revision number. A change such as pausing for your answer or accepting your reply creates another revision. Before saving it, xcb passes the record through a small ALGAL program and keeps the program's input, output, and fingerprints beside the task.

`xcb tasks verify` checks four things while walking backward:

1. Replaying each entry with no host tools reproduces its saved result.
2. The entry agrees with the task record at that revision.
3. Revisions decrease one at a time, retain the task's identity, and reach the expected first entry.
4. Each entry uses the program fingerprint recorded on the task and replays against the program in the installed xcb build.

The verifier reads local records and replays each revision. It neither reruns the task's tests nor asks the provider whether its report was correct. A passing result gives you a consistent task history to review alongside the code and test evidence.

## Check a copy of the state directory

Opening xcb's state directory can apply schema upgrades and retention. Copy it before inspecting a history you need to preserve, then point the verifier at the copy:

```sh
xcb --state /path/to/copied-state tasks verify <task-id>
```

A successful result includes the task ID, the number of revisions replayed, and the fingerprints of the latest entry and its program:

```json
{
  "taskId": "<task-id>",
  "verified": true,
  "revisions": 3,
  "receipt": "sha256:...",
  "policy": "sha256:..."
}
```

A missing entry, mismatched record, or failed replay exits with an error. xcb's tests cover a task that pauses and receives an answer, then fails verification after its stored goal is changed directly in the database. They also check that an existing history still verifies after a schema upgrade.

The verifier stops at 1,024 revisions. Finished tasks older than the 30-day retention window can be removed with their histories, while tasks referenced by live work are retained. Keep a copy before retention removes a history you need to inspect. The [managed harness reference](https://github.com/hraness/xcb/blob/main/docs/managed-harness.md#verification) documents the command and its storage rules.
