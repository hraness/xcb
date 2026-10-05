Excalibur (xcb) tracks how much quota each of your subscriptions has left so it can route the next task. For a day-by-day record of what your coding agents used, it relies on [aicharts](https://aicharts.io/usage), which keeps daily token totals on your computer.

## What the installer sets up

On macOS (Apple silicon) and Linux x86_64, `curl -fsSL https://xcb.sh/install.sh | sh` installs aicharts beside `xcb`. The script checks the download against a SHA-256 digest written into the script and, on macOS, checks the binary's Developer ID signature before running it. On a first install it turns on aicharts' local history. aicharts then reads your agents' session files four times a day and keeps daily totals by agent, provider and model. Nothing is uploaded, and no account is involved.

Set `XCB_USAGE_HISTORY=no` to install aicharts but leave history off, or `XCB_AICHARTS=no` to skip aicharts. Later installs leave the history setting alone, and the installer does not replace an aicharts that is already on your `PATH` somewhere else.

## Reading the record

```sh
xcb usage                                 # the last 30 days, per agent
xcb usage report --days 7 --client claude # one agent, one week
xcb usage report --since 2026-09-01 --until 2026-09-30 --csv > september.csv
```

`xcb usage` runs only aicharts' `history` commands: report, status, enable, disable and collect. It refuses anything else, so xcb cannot ask aicharts to enroll or publish. Quota left on each subscription stays in `xcb accounts`. The two answer different questions: which account can take the next task, and what your agents used last month.

## Letting tasks read it

`xcb usage connect` registers aicharts' read-only MCP server with xcb's host tool bridge. Any Claude or Codex task xcb routes can then ask for totals, a daily series, the full report, the agent list or collection status, and answer questions about your token use or chart it.

The registration pins the aicharts executable's SHA-256, like any `xcb tools add` definition, so xcb runs only the build you connected. Run `xcb usage connect` again after updating aicharts. The xcb installer does that for you when it replaces aicharts, and `xcb doctor` reports a pin that no longer matches. `xcb usage disconnect` removes the tools.

## What the record leaves out

aicharts reads the session files that Claude Code, Codex and other agents keep in their usual folders. The Claude Code and Codex runs that xcb starts leave nothing there, because they run under private profiles inside xcb's state folder, so those runs are not in the daily record. xcb keeps its own token counts for them. With exports on (`xcb plugins enable aicharts-export`), it also writes those counts as session files in aicharts' format, in its state folder. Automatic upload is not available.
