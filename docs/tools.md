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
servers and can hand a task to Codex when it needs your signed-in browser or
native desktop application control. Codex then finishes that task; this is
not a nested desktop-tool session inside Claude or Devin.
An approval denial does not grant permission to try the same action through
another provider.

## Share Claude's Chrome connection

On macOS, install the official Claude Chrome extension and sign in to it.
Then connect the matching Claude account in xcb:

```sh
xcb tools setup-browser
```

Setup opens full Claude sign-in if the account still uses a model-only token.
It always prints the sign-in link, including when the browser opens
automatically. Use the same Claude account as the extension.

Full sign-in requires a dedicated xcb Keychain entry for its refresh
credential. xcb caches the short-lived access token in its private state
folder and refreshes it before use when needed. If Claude cannot save the
refresh credential to Keychain, xcb refuses to
activate that sign-in. Your usual Claude Code sign-in is separate. To
authorize the account explicitly, use
`xcb accounts login NAME --browser`. Subsequent sign-ins keep that account's
full sign-in mode.

Ordinary `xcb setup claude` still uses a model-only token until you connect
the browser. Signing in to the extension again cannot add browser permission
to that token. Full Claude browser sign-in is currently available on macOS;
use the Codex browser connector on other platforms when available.

This connection supports screenshots, page reading, navigation, clicks, and
form input for Codex, Claude, and Devin. It uses the checked Claude Code
2.1.285 browser server. If several Claude accounts are enabled in xcb, use
`xcb tools setup-browser --account NAME` to select the account matching your
extension. xcb holds that account while its browser server runs; the other
providers never receive its credentials. The extension still controls browser
access and permission prompts.

Setup checks the account's browser authorization before starting the browser
server. A failed sign-in preserves the previous account credentials. A failed
connection leaves existing tool configuration intact. Setup saves the
connection only after the extension responds successfully.

If sign-in or refresh is interrupted and xcb keeps the account held, run
`xcb recover` to find the affected run, then `xcb recover RUN_ID` to preview
recovery. Recovery requires proof that the owning xcb process and its sign-in
helpers have stopped. `xcb recover RUN_ID --yes` keeps the saved credentials
and frees a recoverable account; if its active sign-in may have changed, xcb
requires a new `xcb accounts login NAME --browser` before using it again.

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

## Route browser and desktop work

Use an explicit requirement when the task needs an existing signed-in page
or native desktop application control:

```sh
xcb run --signed-in-browser -p 'Read the open account dashboard and summarize its status'
xcb run --desktop -p 'Inspect the open document in the desktop app and summarize it'
```

xcb requires Codex for this work and prefers Astra among available Codex
routes. The requirement stays with the session across retries, account
changes, and resumptions. A conflicting Claude or Devin pin reports that
conflict instead of silently changing the pin. If no suitable Codex account
is available, the task waits or reports the unavailable route.

The configured routing judge also checks whether the task requires an
existing signed-in browser or native desktop control, independently of its
coding difficulty. An uncertain or unavailable judgment does not invent
either requirement. A provider that discovers the need during
work can declare `signed_in_browser` or `desktop` through
`xcb_require_capability`; xcb settles and joins the current provider before
handing the same conversation and workspace to Codex. Denials, outstanding
approvals, cancellation, or uncertain effects prevent this handoff. An explicit one-turn route
still returns after that turn.

Ordinary Playwright checks in a fresh browser, public-page research, and
work on login or OAuth code use normal routing. They do not require the
signed-in-browser flag.

Desktop work requires the native connector's explicit `computer` surface.
Shared Chrome controls and a browser-only native connector do not satisfy
that requirement. Missing desktop support is reported before native effects.
Setup and configuration readiness do not replace operating-system permission
or live qualification of a particular desktop app.

Using native Codex tools also preserves a Codex route for subsequent turns
and managed retries, without assuming the operation involved a signed-in
website or desktop app. This host-recorded route requirement is separate
from the two declared task capabilities.
