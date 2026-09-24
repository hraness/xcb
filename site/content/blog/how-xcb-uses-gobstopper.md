Two hours into a refactor, much of what your coding agent receives each turn can be old tool output: the files it read at the start, a search it has already acted on, the log from a test run it fixed an hour ago. The instructions you gave and the decisions it made are still in there, but they share the space with pages of text nobody needs again.

xcb handles this for Claude Code and Codex sessions with Gobstopper. When a session grows past a threshold, xcb applies Gobstopper's elision policy to the prompt it is about to send: old tool results become a one-line marker, the recent work stays word for word, and the original output stays in xcb's local history. It is on by default. Latest release: {{release.version}}.

## Long sessions fill up with output nobody rereads

A coding agent works by reading. It opens files, runs searches, lists directories and runs tests, and each result lands in the conversation. The model usually needs the conclusion from that output, which it already wrote down in its own reply, and rarely needs the raw text again.

That raw text still counts against the context window. As a session grows, something eventually has to be cut or summarized, and the parts worth keeping are what you asked for, what was decided and the last few results.

## What Gobstopper is

[Gobstopper](https://gobstopper.sh) is a tool for inspecting Claude Code, Codex and Devin sessions and preparing smaller copies of them. Its simplest rule is called elide: replace old tool outputs with a short stub, oldest first, and leave the newest ones alone. It needs no model to decide what to cut, so the same session and the same settings always give the same plan.

Used on its own, Gobstopper also keeps a local archive of each transcript before it prepares a compacted copy, so you can search old sessions and read back a specific archived record when a summary leaves it out.

xcb uses a smaller piece: Gobstopper's core library, pinned to a specific commit in xcb's build, and its elide strategy. On this path xcb neither runs the Gobstopper program nor uses its archive; it keeps its own history.

## How xcb applies the policy each turn

xcb stores every message of a session locally and builds each turn's prompt from that history. Gobstopper runs during that build, in these steps:

1. **Estimate the size.** xcb estimates the prompt at roughly one token per four bytes of text. If the estimate is below the trigger (250,000 by default), nothing changes.
2. **Ask Gobstopper for a plan.** xcb hands Gobstopper a list of the messages, marking only tool results as removable. Gobstopper's elide strategy walks those results from oldest to newest, skips the newest eight, and stops once the estimate would fall to the floor (40,000 by default).
3. **Check it is worth doing.** The plan is used only if it saves at least the minimum (4,096 estimated tokens by default).
4. **Protect the recent tail.** xcb separately refuses to touch any of the last eight messages, whatever their role.
5. **Rewrite the outgoing copy.** Each chosen tool result is replaced in the outgoing prompt by a marker that says how many bytes were removed. The saved history keeps the original.

In outline, the rule looks like this:

```text
if estimated_size < trigger:
    send the history as it is
else:
    candidates = tool results, oldest first, except the newest 8
    stub candidates until estimated_size <= floor
    use the plan only if it saves at least min_savings
    never touch the last 8 messages, or any user or assistant text
```

In place of each old result, the model sees this marker:

```text
[output elided by gobstopper: <bytes> bytes; original retained in local history]
```

When xcb elides anything, it tells you in the session: "Gobstopper elided N stale tool outputs in the prompt; history is retained."

Three rules hold at any settings. Your messages and the agent's replies are never rewritten, only tool output. The saved session is never edited; each turn starts again from the saved history. And with the judge off, the same history and settings give the same prompt, because Gobstopper's rule uses no model.

### An optional second opinion

Some old results still matter, such as an exact error message or a file the agent is about to edit again. If you have turned on xcb's optional judge (off by default), xcb asks it one yes-or-no question per candidate: does the next turn still need this output word for word, where running the tool again would not do? The judge is an external judgment service, which is why it is opt-in. It sees the tool's name, the output's size, your current task and a limited excerpt of the recent conversation. It does not see the tool output itself. An output it wants to keep, with a probability of 0.5 or more, stays in the prompt.

The judge can only keep things. It never adds a candidate Gobstopper did not choose. xcb asks about at most 64 candidates per turn, and any beyond that stay in full. If the judge is unavailable, fails or leaves an answer out, xcb says so in the session and falls back to Gobstopper's plan on its own. After the judge answers, xcb checks the minimum saving again and trims nothing if what remains falls below it.

### Tuning or turning it off

The settings sit under `extensions.gobstopper` in xcb's `config.json`, and `xcb config` shows the values in effect. The defaults are:

```json
{
  "extensions": {
    "gobstopper": {
      "enabled": true,
      "trigger_tokens": 250000,
      "floor_tokens": 40000,
      "min_interval_ms": 300000,
      "min_savings_tokens": 4096
    }
  }
}
```

xcb rejects out-of-range values: for example, a floor below 1,024 or not below the trigger, or a trigger above 1,000,000. To switch the feature off, or back on:

```sh
xcb plugins disable gobstopper
xcb plugins enable gobstopper
```

## What changes for you

- **Long sessions keep going without manual cleanup.** Once a session crosses the trigger, stale output drops out of the prompt without you running a command or starting a fresh session.
- **What you said and what was decided stay put.** Your instructions and the agent's replies go into every prompt as written, and the newest results stay in full.
- **The originals stay in local history.** The saved session still has every original tool result, so the history you review, export or replay is complete.
- **The same input gives the same trim.** Without the judge, the same session and settings always trim the same messages. With the judge, it can only keep more.

## Where it stops

This covers Claude Code and Codex sessions that xcb runs. Devin sessions are sent without elision. The sizes are estimates from byte counts, not the provider's token counts, and xcb reports no measured savings or effect on your bill. A smaller prompt is also not proof that the next turn goes better; Gobstopper's own documentation makes the same point about its compaction. Once an output is elided, the model sees only the marker, so if it needs that text again it has to run the tool again. You can still read the original in xcb's history.

The rest of xcb is covered in [Introducing xcb](/blog/introducing-xcb), and Gobstopper as a standalone tool at [gobstopper.sh](https://gobstopper.sh).
