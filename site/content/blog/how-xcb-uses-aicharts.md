Excalibur (xcb) measures how much quota each subscription has left so it can route your next task. It does not keep a history of what your agents spent. For that it uses [aicharts](https://aicharts.io/usage), which keeps a daily record of token use on your computer, for the tasks xcb runs and for every other agent you use.

## What the installer sets up

On macOS (Apple silicon) and Linux x86_64, `curl -fsSL https://xcb.sh/install.sh | sh` installs aicharts beside `xcb`. The script checks the download against a SHA-256 digest written into the script and, on macOS, checks the binary's Developer ID signature before running it. On a first install it turns on aicharts' local history. aicharts then reads your agents' session files four times a day and keeps daily totals by agent, provider and model. Nothing is uploaded, and no account is involved.

Set `XCB_USAGE_HISTORY=no` to install aicharts but leave history off, or `XCB_AICHARTS=no` to skip aicharts. A later install never changes the choice you made, and an aicharts you installed some other way is left alone.

## Reading the record

```sh
xcb usage                                 # the last 30 days, per agent
xcb usage report --days 7 --client claude # one agent, one week
xcb usage report --since 2026-09-01 --until 2026-09-30 --csv > september.csv
```

`xcb usage` runs only aicharts' `history` commands: report, status, enable, disable and collect. It refuses anything else, so xcb cannot ask aicharts to enroll or publish. Quota left on each subscription stays in `xcb accounts`. The two answer different questions: which account can take the next task, and what your agents spent last month.

## Letting tasks read it

`xcb usage connect` registers aicharts' read-only MCP server with xcb's host tool bridge. Any Claude or Codex task xcb routes can then ask for totals, a daily series, the full report, the agent list or collection status, and answer questions about your token use or chart it.

The registration pins the aicharts executable's SHA-256, like any `xcb tools add` definition, so xcb runs only the build you connected. Run `xcb usage connect` again after updating aicharts. The xcb installer does that for you when it replaces aicharts, and `xcb doctor` reports a pin that no longer matches. `xcb usage disconnect` removes the tools.
