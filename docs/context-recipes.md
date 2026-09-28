# Inspect source snapshots with ALGAL

`xcb context` saves selected source files, finds excerpts for up to four
subquestions, and builds a resumable ALGAL program. Each subquestion runs as an
ordinary xcb coding task. A final task combines the reports with their citations.
The program releases its workspace and account while waiting for a child, so it
can use a single subscription account without waiting on itself.

This feature is an experimental retrieval workflow. Its local term-match
measurements compare selected excerpts with an equally sized prefix of the source;
they do not establish answer quality or subscription savings. Selection is lexical,
so it can miss relevant material that uses different words.

The [recorded coordination diagnostic](../examples/context-recipes/coordination/README.md)
includes a complete offline replay, both stopped attempts, subscription usage,
and the citation errors found in its answers. It is an evaluation example, not
evidence of a quality improvement.

## Prepare a recipe

Create a plan with workspace-relative source paths and concrete search terms:

```json
{
  "question": "How are uncertain runs recovered, and which evidence is retained?",
  "paths": ["src/runtime.rs", "docs/recovery.md"],
  "subquestions": [
    "uncertain recovery process exit",
    "retained result receipt replay"
  ]
}
```

Run these commands from the source workspace. The output directory is private
because the recipe retains the complete selected files. Use a new directory for
each recipe; an existing recipe is never overwritten.

```sh
xcb context prepare plan.json --output /absolute/private/context-study
xcb context inspect /absolute/private/context-study/recipe.json
xcb context inspect /absolute/private/context-study/recipe.json --chunk DIGEST
```

Plan, recipe and result inputs must be regular files; symlinks and special files
are rejected before reading.

Preparation makes no provider calls. It accepts at most 64 UTF-8 files, 2 MiB per
file and 8 MiB total. Source reads reject symlinks, hardlinks, paths outside the
workspace, and the credential and generated-file paths excluded by xcb's command
snapshots. Select only files you intend to share with the provider: these exclusions
cannot recognize a secret embedded in an ordinary source file.

Chunks contain at most 1,536 bytes. Each subquestion selects up to two chunks.
Every address records the source digest and exact byte range; inspecting the
saved recipe reads those bytes even if the working file later changes. The recipe
verifies its program, selections and source hashes together before use.

## Run and replay

The project needs an active task grant with enough remaining tasks for every
subquestion and the final answer. Configure the grant with `xcb projects configure`
and inspect it with `xcb projects`. Submitting a recipe cannot create or renew a
grant, select new provider credentials, or change the provider's tools.

```sh
xcb backlog context /absolute/project /absolute/private/context-study/recipe.json
xcb backlog program-status TASK_ID
xcb context replay /absolute/private/context-study/recipe.json --task TASK_ID
```

Children use the project's ordinary provider selection and account controls.
One to four subquestions produce two to five coding tasks. Inspection results
share a combined 6 KiB limit, divided equally among the subquestions and reduced
when needed to fit the final request. These limits count canonical JSON bytes,
including quotes and escaped characters. For a short main question, two
inspections receive 3,072 bytes each and four receive 1,536 bytes each.

Preparation checks every inspection request and the largest possible synthesis
request against the 8 KiB request limit. It rejects plans that cannot allow at
least 1,024 bytes per inspection. Prompts still request concise 600-byte reports;
the enforced allocation allows additional room. Synthesis has a 4,608-byte JSON
output limit. A result that exceeds its allocation stops the program; reports
are never silently truncated. Prompts ask for source-based answers and no file
edits; they do not turn the coding provider into a read-only provider.

The existing attention and cancellation commands apply to the child tasks.
An uncertain child stays held until xcb can establish how its process ended.
The program does not retry it. Preparation and offline replay do not require a
signed-in provider; execution requires an eligible, supported provider build.

Replay uses the completed task's original results and checks its final record.
For experiments, `--results results.json` accepts an array of `requestDigest` and
`summary` objects instead. These supplied results are evidence of deterministic
control flow, not evidence that a provider produced them or that an answer is true.

Use each child's normal task and session records for usage. Subscription limits,
reported tokens and elapsed time remain separate from API prices; this workflow
does not assign fictional per-token charges to a subscription.

An embedding host can call `xcb context next recipe.json --results results.json`
to get the next request after replaying a recorded prefix. Omit `--results` for
the first request. This command never starts a worker. The host must send requests
through its supported xcb route, retain the original response, confirm that the
run completed, and stop on an uncertain outcome before supplying the next result.
