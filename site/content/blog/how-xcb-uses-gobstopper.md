Long coding sessions accumulate tool output: files read before an edit, searches that located a function, and test logs from an earlier failure. Some of that text remains useful, but carrying every result into every turn takes space away from the work at hand.

xcb uses [Gobstopper](https://gobstopper.sh) to shorten old tool results in the outgoing prompts for Claude Code and Codex. The original results remain in xcb's saved history, and recent messages stay intact.

## Shorten the copy sent to the model

Gobstopper can inspect sessions and prepare smaller copies of them. xcb uses its core library's elide rule: replace selected old tool outputs with a short marker, starting with the oldest. That selection needs no model call.

xcb builds each turn's prompt from its stored session. When its estimated size crosses the configured threshold, it asks Gobstopper which tool results could be shortened. The policy protects recent results, and xcb separately protects the newest messages regardless of their role. User instructions and assistant replies are preserved.

A selected result becomes a marker in the outgoing prompt:

```text
[output elided by gobstopper: <bytes> bytes; original retained in local history]
```

The next turn is built from the original history again. xcb does not replace the saved results, and this integration does not use the standalone Gobstopper archive. You can still inspect the full output in xcb's own session history.

## Ask a judge to retain an output

An exact error message or a file the agent will edit again may deserve to stay. If you enable xcb's optional judge, xcb asks it whether to retain a candidate output.

The judge receives the tool's name, the output size, the current task, and a limited excerpt of recent conversation. It does not receive the tool output itself. It can only retain results from Gobstopper's proposed selection; it cannot add another result to remove.

If the judge is unavailable or its response fails, xcb reports that and falls back to the deterministic plan. After the judge responds, xcb checks the saving again and leaves the prompt unchanged when too little would be removed.

## Choose when shortening starts

Gobstopper is enabled by default for the Claude Code and Codex sessions xcb runs. All supported provider sessions use this elision. The settings under `extensions.gobstopper` control the size trigger, target size, and minimum saving. `xcb config` shows the current values; the [customization guide](/docs/customization) covers configuration.

To disable or enable it:

```sh
xcb plugins disable gobstopper
xcb plugins enable gobstopper
```

The size estimate comes from text bytes, rather than a provider's exact token count. It helps decide when to shorten a prompt; it does not measure a change in the provider's bill or the quality of the next answer.

After a result is elided, the model sees its marker. If the next step needs the full text, the agent has to retrieve it again. Keeping the saved original makes that loss from the prompt inspectable instead of permanent.
