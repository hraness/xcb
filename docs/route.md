# Route one task through xcb

`xcb --json route` lets another program, usually a coding agent, hand xcb one
task. xcb picks an account and model that can take it, runs one provider turn
in the project folder you name, and prints one JSON result. The caller can
narrow the choice; checking the provider build, holding the account, keeping
the provider inside the folder, and confirming the provider stopped all stay
with xcb.

Three commands run one task at a time:

- `xcb run` is for people: flags on the command line, a saved direct session,
  and your configured continuation and [failover](failover.md).
- `xcb --json route` is for programs: one JSON request on stdin, account and
  model chosen per request, exactly one provider turn, no continuation.
- `xcb --json generate` is for applications: one model response with no tools,
  folder, or session. See the [application API](application-api.md).

## Request

Write one UTF-8 JSON document to stdin, close stdin, and read the result from
stdout. The request is limited to 1 MiB. Unknown fields are rejected; every
field except `version`, `workspace`, and `task` is optional.

```json
{
  "version": 1,
  "workspace": "/absolute/path/to/project",
  "task": "Fix the failing parser test and show the diff",
  "provider": "claude",
  "account": "a_…",
  "model": "claude/sonnet/low",
  "timeoutMs": 1800000,
  "dryRun": false
}
```

- `version` is `1`. Pin it: a changed request format ships under a new version.
- `workspace` must be an existing folder. The provider's file tools stay inside
  it.
- `task` is 1 byte to 256 KiB of text without NUL.
- `provider` is `claude` or `codex`, and requires that provider.
- `account` names one account by ID or exact name. A `provider` that doesn't
  match the account's provider is an `invalid_request`.
- `model` is a full key as printed by `xcb models`, and limits the route to
  that model.
- `timeoutMs` is 1,000 to 3,600,000. When it expires, xcb cancels the turn and
  answers only after the provider has stopped; the code is `deadline`.
- `dryRun: true` reports the route without creating a session, holding an
  account, or starting a provider.
- `requirements: {"signed_in_browser": true}` requires Codex for an existing
  signed-in browser; `requirements: {"desktop": true}` requires Codex for
  native desktop application control. Both can be set together. Requirements
  persist with the saved session, and a conflicting provider, account, or
  model pin is rejected. See
  [browser and shared tools](tools.md) for setup and handoff behavior.

Pins limit the choice; xcb never falls back outside them. With no pins, xcb
considers accounts with a supported provider build that are signed in,
enabled, idle, and not at a known usage limit, with a model recently seen in
the provider's catalog. It orders those models by your
[preference stack](quota-routing.md#preference-stack), then by task type,
relative quality, cost, and latency, remaining usage, and your configured
favorites. An optional judge can require browser or desktop capabilities and
rank eligible routes; it preserves your pins and the provider checks above.
A pinned `model` that the stack's `never` list excludes fails with
`unavailable`. See [quota routing](quota-routing.md) for the rules.

## Response

Top-level fields are camelCase; the fields inside `outcome` are snake_case.
A chosen route reports `provider`, `account`, the full `model` key, a display
`label`, and a short `reason` for a person to read: how xcb classified the
task, the capability tier (`standard` or `frontier`), the model's relative
quality, cost, and speed, and the preference-stack tier and pattern position
that decided (`tier default · stack #1`). The route object has no other
fields; the stack tier is reported only inside `reason`. It explains the
choice and is not a price or quality guarantee. A public pricing promotion is named in the reason but never changes
which route wins. A dry run returns:

```json
{
  "version": 1,
  "status": "selected",
  "requestId": "route_…",
  "route": {
    "provider": "claude",
    "account": "a_…",
    "model": "claude/sonnet/low",
    "label": "Sonnet · low",
    "reason": "deterministic fallback · classifier not available · standard tier · balanced task · Pareto P1 · quality 92 · relative cost 55 · relative latency 50"
  }
}
```

A run returns `status: "completed"` only when the turn completed, the provider
process has exited, xcb has recorded its effects, nothing is waiting for an
answer, and the turn produced answer text or file changes:

```json
{
  "version": 1,
  "status": "completed",
  "requestId": "route_…",
  "session": "s_…",
  "route": { "provider": "claude", "account": "a_…", "model": "claude/sonnet/low", "label": "Sonnet · low", "reason": "…" },
  "state": "idle",
  "outcome": {
    "terminal": "completed",
    "joined": true,
    "effects": "settled",
    "pending_attention": false,
    "failure": null
  },
  "text": "…"
}
```

In `outcome`, `joined: true` means the provider's processes have exited, and
`effects` is `none`, `settled` (changes recorded), or `uncertain`. `session`
reopens with `xcb resume <session>`; the routed turn is an ordinary saved
direct session. `text` holds up to 256 KiB, and `textTruncated: true` marks a
longer answer.

## Failures

A failure exits 1 and prints one object:

```json
{
  "version": 1,
  "status": "failed",
  "requestId": "route_…",
  "code": "unavailable",
  "joined": true,
  "effects": "none"
}
```

| Code | Meaning |
| --- | --- |
| `invalid_request` | The request is malformed, the folder doesn't exist, or stdin is a terminal. |
| `unavailable` | No account can take the task: none qualify, the account or model is unknown, the provider build isn't supported, or credentials are missing. |
| `busy` | The account is running another task. |
| `deadline` | The caller's `timeoutMs` expired and the turn was cancelled. |
| `cancelled` | SIGINT or SIGTERM cancelled the turn. |
| `provider_error` | The turn failed, hit a provider limit, or ended without a reply or file changes; `outcome.terminal` and `outcome.failure` carry the detail, such as `account_quota`, `model_quota`, or `no_reply`, and a person can reopen `session`. |
| `custody_unproven` | xcb couldn't confirm that the provider stopped or what it changed, so it keeps the account held. Don't retry blindly; see `xcb recover`. |
| `needs_input` | The provider stopped with a question; `text` carries it, and a person can reopen `session`. |

`joined: true` and `effects: "none"` appear only when the request provably
started no provider process. Once a session exists, those facts come from the
recorded `outcome` instead. SIGINT and SIGTERM cancel the turn the same way
`timeoutMs` does; killing xcb doesn't prove the provider stopped.

## Notes for agents

- One call is one turn. Multi-step plans, retries, and route exclusion are the
  caller's loop; an account that failed on a usage limit is skipped on the
  next call because xcb recorded the limit.
- A request never carries tools, hooks, system prompts, credentials, or
  provider flags. The provider gets the same file tools as any direct session.
- Read accounts, models, usage limits, and provider status with
  `xcb --json accounts`, `xcb --json models`, and `xcb --json doctor`.
  `xcb --json models route --task …` previews the route the thread would pick.
