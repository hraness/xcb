# Security

xcb runs each provider CLI in an operating-system sandbox with xcb's file
tools for one project folder, keeps credentials outside projects, and holds each
account for one task until the provider process exits. It runs only provider
builds whose executable it has checked; the
[supported builds](https://xcb.sh/docs/providers#supported-builds) and
[security and privacy](https://xcb.sh/docs/security) pages describe what that
covers. A model list, a passing metadata check, or a synthetic test is not a
security guarantee for a provider build. The `xcb-compat` CLI keeps its Codex
task routes disabled.

Owner-registered host MCP servers run outside the provider sandbox and can
access resources beyond the project according to their own permissions.
Their environments and credentials are not copied into the provider process.
The installed desktop computer-use connector preserves automatic approval
review through Codex. See [tool setup and access](docs/tools.md).

Report another task taking over a held account, input that escapes its size
limits, network or credential access the sandbox should block, sandbox escapes,
unsafe replay, and confusion about which process holds an account.

## Reporting

Open a private security advisory on
[hraness/xcb](https://github.com/hraness/xcb/security/advisories/new), email
[hraness@pm.me](mailto:hraness@pm.me), or use the
maintainer contact listed on the organization profile. Include a minimal
reproduction when possible. Remove account keys, private paths, provider state,
and transcript contents from diagnostic attachments.

Do not open a public issue for an unpatched vulnerability.
