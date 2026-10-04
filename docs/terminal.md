# Agent and SDK interface

The interactive Ratatui terminal surface has been removed. xcb is now driven
through its headless command and protocol surfaces so another agent, a script,
or an on-demand UI can own the interaction.

Use one of these entry points:

```sh
# One local task with human-readable output
xcb run -p "Fix the failing parser test"

# Versioned JSON route for another agent
printf '%s\n' '{"version":1,"workspace":"/absolute/project","task":"Fix the failing parser test"}' \
  | xcb --json route

# Durable task inspection and projections
xcb --json tasks
xcb --json attention
```

The JSON route is the stable boundary. It validates the request, selects an
eligible subscription, runs one bounded turn in the requested workspace, and
returns a typed result with a request ID, route, outcome, custody, and effects.
See [the route contract](route.md), [the SDK](sdk.md), and [the north-star
vision](vision.md) for schemas, receipts, event projections, and the planned
external UI protocol.

Saved task, conversation, account, and receipt state remains local under
`~/.local/share/xcb` (or `XCB_STATE`). A caller that needs a view should build
it from `--json` projections or the SDK; xcb does not ship a terminal UI.
