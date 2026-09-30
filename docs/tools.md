# Browser, computer, and shared tools

The current source build of xcb can connect host MCP tool servers to Claude, Codex, and Devin. Registered
shared tools are available through each provider's xcb tool bridge.
Screenshots from shared MCP servers are stored privately with the session
and can be reopened after a provider handoff.

## Connect computer use

On macOS, with the desktop computer-use plugin installed:

```sh
xcb tools setup-computer
xcb tools list
```

The installed desktop connector uses Codex's automatic approval reviewer.
Its native tool results remain within the Codex run.
xcb checks the installed application and copies its runtime into a private
tool directory before connecting it. A changed or unsupported installation
must be connected again. Your browser profiles and account sign-ins stay in
their existing locations.

This connector is available to Codex. Claude and Devin can use shared MCP
servers and can hand a task to Codex when it needs your signed-in browser.
An approval denial does not grant permission to try the same action through
another provider.

## Connect Chrome for every provider

With the official Claude Chrome extension installed and signed in:

```sh
xcb tools setup-browser
```

This connection supports screenshots, page reading, navigation, clicks, and
form input for Codex, Claude, and Devin. It uses the checked Claude Code
2.1.285 browser server. If several Claude accounts are signed in to xcb, use
`xcb tools setup-browser --account NAME` to select the account matching your
extension. xcb holds that account while its browser server runs; the other
providers never receive its credentials. The extension still controls browser
access and permission prompts.

Setup prepares the extension's browser group and leaves it open between
tasks. It preserves an existing group, or leaves the new group's initial blank
tab open. If you close that group, run setup again. Each task closes only tabs
it created and preserves the group's existing tabs.

Batched browser actions and shortcut execution are not exposed because they
do not reliably identify newly created tabs. Desktop application control uses
the Codex connector above. A registered connection still needs a running,
signed-in extension and any required operating-system permissions.

## Connect a shared MCP server

Use `xcb tools add /absolute/path/to/definition.json` to register a server
you trust. `xcb tools list` shows the registered servers and the providers
that can use them; `xcb tools remove NAME` removes a registration.

A definition names the server, its absolute executable path, and that
executable's SHA-256. `args` supplies its arguments. `env` names environment
variables to forward; `environment` contains nonsecret settings. Avoid
putting credentials in the definition. Optional `tools` limits the exposed
tool names; `features` can include `browser` and `computer`.

xcb starts each server for the task, checks its tool list, and waits for its
work and process to stop before releasing the account. Host tools can access
resources outside the project, according to the server's own permissions.
Only register servers whose behavior and access you intend to grant.
Providers do not inherit those servers' environments or credentials.

Native provider shells and unrelated plugins remain separate from the
shared tool bridge. See [provider permissions](provider-permissions.md) and
the [command runner](command-runner.md).

## Route signed-in browser work

Use an explicit requirement when the task needs an existing signed-in page:

```sh
xcb run --signed-in-browser -p 'Read the open account dashboard and summarize its status'
```

xcb requires Codex for this work and prefers Astra among available Codex
routes. The requirement stays with the session across retries, account
changes, and resumptions. A conflicting Claude or Devin pin reports that
conflict instead of silently changing the pin. If no suitable Codex account
is available, the task waits or reports the unavailable route.

The configured routing judge also checks whether the task requires an
existing signed-in browser. An uncertain or unavailable judgment does not
invent a browser requirement. A provider that discovers the need during
work can declare it through `xcb_require_capability`; xcb finishes and stops
the current turn before handing off to Codex. An explicit one-turn route
still returns after that turn.

Ordinary Playwright checks in a fresh browser, public-page research, and
work on login or OAuth code use normal routing. They do not require the
signed-in-browser flag.
