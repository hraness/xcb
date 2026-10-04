# Provider approval modes

Excalibur (xcb) starts native Claude Code in Auto mode and Codex with
automatic approval review. Each provider runs in a private profile and uses
xcb's workspace tools. Automatic review does not expand the files or commands
those tools can access.

Claude must report `permissionMode: auto` when it starts. Codex must report
`approvalPolicy: on-request` and `approvalsReviewer: auto_review`, with its
read-only provider sandbox intact. A provider that reports a different mode
cannot start work. xcb does not silently switch either provider to manual
approval or unrestricted execution.

Codex's automatic reviewer handles requests that would otherwise need a
person, as described in the [official Auto-review documentation](https://learn.chatgpt.com/docs/sandboxing/auto-review).
Registered host tools are available through the
[shared tool bridge](tools.md). The installed desktop computer-use connector
uses Codex's native MCP connection so its approval requests reach the
automatic reviewer. A request for additional human approval stops automatic
work; xcb does not approve it on the person's behalf. Unregistered native
tools remain unavailable.
Provider account or organization restrictions can make an approval mode
unavailable; xcb cannot grant access the provider has withheld.

Devin uses its CLI `auto` setting and ACP `accept-edits` mode. xcb answers
allowed workspace-tool requests automatically for one call at a time. Devin
bypass is not enabled: the pinned 3000.11.3 build executed a native web request
despite a deny rule, and its separate tool-disable setting did not disable the
native `skill` tool. The
[bypass test results](../qualification/devin-bypass-3000.11.3-assessment.json)
record both failures for that exact executable.

## Native execution direction

The [north star](vision.md#native-provider-execution) calls for native file,
shell, and network tools for Claude Code, Codex, and Devin. On macOS,
`workspace_native_exec` runs commands in the granted worktree through each
provider's native tool protocol. It uses an OS policy that allows DNS and
outbound TCP 443, clears the inherited environment, and supplies a private
command home. Host GitHub credentials require a separate workspace option
and a successful authenticated test. Provider-built-in host shell and web
tools remain disabled; the host bridge does not claim that a provider's
built-in command classifier reviewed its arguments.

A task with `native_execution: true` preserves that requirement through resume
and provider changes. The host must first pass `xcb native qualify`, run
`xcb native verify --provider <provider>` for each provider, and select the
workspace with `xcb native grant`. Verification uses an existing account
lock and a disposable workspace, authorizes one command, and records tool
results rather than trusting the provider's answer. Qualification does not
enable real project access. Native tasks cannot use offline replay or the
legacy Codex host command tool. Linux and Windows refuse native commands.

Native backends must retain each provider's approval controls and verify their
reported mode. A native shell requires fresh filesystem, network, cancellation,
and recovery evidence. Enabling a tool or changing a configuration flag cannot
reuse the broker-only evidence. Devin's native approval contract needs its own
qualification; the CLI's `auto` alias and ACP `accept-edits` mode do not provide
Claude's Auto classifier or Codex's automatic reviewer.

## When a provider denies an action

A permission denial stops automatic continuation and provider switching.
Claude reports these decisions through `system/permission_denied` events and
the result's `permission_denials` list. xcb records the stop even if Claude
labels the overall result a success, and shows a fixed notice without copying
the denied command or its arguments into diagnostics. A later empty denial
list does not erase a denial already observed during the turn.

Codex's policy errors, including its automatic review rejection limit, also
stop the task. xcb does not treat a model's refusal text as evidence of an
account usage limit, or retry a denied action through another provider.

Account and model usage limits are different: xcb can
[switch subscriptions](failover.md) after the provider process exits and the
previous turn's changes are accounted for. The configured model preferences,
account locks, attempt limits, and explicit provider choices still apply.

Steps requiring a sign-in, a payment, or a fact only the owner knows remain
requests for the owner. See [automatic continuation](reflexes.md).

## Delegated agents

Provider child agents are disabled. Selecting `ultra` reasoning effort does
not enable them.

The pinned Codex 0.159.0 executable showed two incompatibilities: children
without inherited history lacked xcb's workspace tools, and full-history forks
could not start from the temporary root session because it has no saved
rollout. A separate probe also observed a grandchild beyond the configured
depth limit. The [full-history](../qualification/codex-0.159.0-delegation-all.json)
and [depth](../qualification/codex-0.159.0-delegation-depth.json) reports retain
the observed behavior.

The [Claude observation](../qualification/claude-2.1.285-auto-denial.json)
confirms Auto mode and the denial wire format. Its synthetic endpoint does
not implement the approval classifier's replies, so no child started. It
records the resulting denial messages. xcb rejects events
that identify a child agent.
