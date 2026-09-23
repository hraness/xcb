# Route one task through xcb

`xcb --json route` is the machine contract for another program — typically a
coding agent — to hand xcb one task and get back one settled, routed turn. The
caller chooses eligibility constraints only. Provider admission, account
custody, workspace confinement and process settlement stay with the runtime.

This is a different surface from `xcb run` and `xcb --json generate`:

- `run` is the human-facing one-shot: flags on the command line, saved direct
  session, configured continuation and failover behavior.
- `route` is the agent-facing one-shot: a closed JSON document on stdin,
  account/model selection performed per request, exactly one provider turn,
  no continuation.
- `generate` is the application-facing contract: bounded ephemeral text with
  no tools, workspace, or session at all.

## Request

Write one UTF-8 JSON document to stdin, close stdin, read bounded stdout.
Unknown fields are rejected; every field except `version`, `workspace` and
`task` is optional.

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

- `workspace` must be an existing directory. The routed turn's brokered file
  tools stay inside it.
- `provider` is a hard constraint: `claude`, `codex`, or `devin`.
- `account` names one account by id or exact observed name. A `provider` that
  disagrees with the account's provider is an `invalid_request`.
- `model` is a full observed key as printed by `xcb models`; it restricts the
  route to that model alone.
- `timeoutMs` is 1,000–3,600,000. On expiry the turn is cancelled and the
  response is emitted only after custody settles; the code is `deadline`.
- `dryRun: true` selects and reports the route without creating a session,
  reserving an account, or launching a provider.

With no pins, routing chooses among admitted runtimes, credentialed enabled
accounts that are idle and not within a known quota-blocked window, and observed
fresh model entries. Candidates are scored by task class, relative quality,
cost and latency Pareto layers, remaining usage, configured favorites, and an
optional judge that can only order already-eligible routes. See
[quota routing](quota-routing.md) for the selection rules.

## Response

A selected route reports `provider`, `account`, the full `model` key, a display
`label`, and a bounded heuristic `reason` (deterministic or judge-selected, task
class, Pareto layer, relative profile). A dry run returns:

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
    "reason": "deterministic · balanced task · Pareto P1 · quality 92 · relative cost 55 · relative latency 50"
  }
}
```

An executed route returns `status: "completed"` only for a completed, joined,
settled turn with no pending attention:

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

`session` reopens with `xcb resume <session>`; the routed turn is an ordinary
saved direct session. `text` is bounded at 256 KiB and flagged with
`textTruncated` when it exceeded the bound.

Failures return a nonzero exit code and a closed object:

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

Codes are `invalid_request`, `unavailable` (no eligible route, unknown account
or model, unadmitted runtime, missing credentials), `busy` (account custody
held by live work), `deadline` (the caller's `timeoutMs` expired), `cancelled`,
`provider_error` (the turn failed or hit a provider limit; `outcome.terminal`
and `outcome.failure` carry the exact detail, including `account_quota` /
`model_quota`), `custody_unproven` (process exit or effect settlement could not
be proven — the account record stays held; do not retry blindly), and
`needs_input` (the provider stopped with a question; `text` carries it and the
saved `session` can be resumed by a person).

`joined: true` and `effects: "none"` appear only when the request provably
launched no provider process. Once a session exists they come from the recorded
`outcome` instead. SIGINT and SIGTERM request the same bounded cancellation and
settlement path as `timeoutMs`; killing xcb does not prove the provider stopped.

## Notes for agents

- A `route` call is one turn. Multi-step plans, retries and route exclusion are
  the caller's loop; quota-failed accounts are excluded by their own recorded
  evidence on the next call.
- The contract never accepts tools, hooks, system prompts, credentials, or
  provider flags. Workspace tools are the brokered set every direct session
  gets; nothing else crosses the boundary.
- Discovery of accounts, models, quota state and provider admission uses the
  existing read-only surfaces: `xcb --json accounts`, `xcb models`,
  `xcb --json doctor`, and `xcb --json models route --task …` for the managed
  intake preview.
