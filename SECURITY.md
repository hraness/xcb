# Security

xcb runs each provider CLI in an operating-system sandbox with only xcb's file
tools for one project folder, keeps credentials outside projects, and holds each
account for one task until the provider process exits. It runs only provider
builds whose executable it has checked; the
[supported builds](https://xcb.sh/docs/providers#supported-builds) and
[security and privacy](https://xcb.sh/docs/security) pages describe what that
covers. A model list, a passing metadata check, or a synthetic test is not a
security guarantee for a provider build. The `xcb-compat` CLI keeps its Codex
and Devin task routes disabled.

Report another task taking over a held account, input that escapes its size
limits, network or credential access the sandbox should block, sandbox escapes,
unsafe replay, and confusion about which process holds an account.

## Reporting

Open a private security advisory on
[hraness/xcb](https://github.com/hraness/xcb/security/advisories/new), or use the
maintainer contact listed on the organization profile. Include a minimal
reproduction when possible. Remove account keys, private paths, provider state,
and transcript contents from diagnostic attachments.

Do not open a public issue for an unpatched vulnerability.
