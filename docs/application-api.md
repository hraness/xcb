# Build applications with an AI subscription

XCB owns coding-agent sign-in, provider confinement and process/account custody.
An application supplies a bounded prompt and receives untrusted text. Its own
code decides which files, network operations or messages that text can propose.
[Textbutler](https://github.com/hraness/textbutler) is the reference consumer: it
uses separate classification and reply prompts and keeps contact memory,
recipient selection, review and message delivery in its trusted host.

The application route is separate from `xcb run`. It creates no saved session,
reads no conversation history and enables no tools, hooks, plugins, judge,
continuation or account fallback. Private run records preserve account/process
custody, selected model and timing without storing application prompts or replies.
Provider authentication remains in XCB; applications never pass credentials.

An installation reports `supported: false` until the application path has current
qualification for the exact built executable, provider, account and model. A provider pin,
configured account or successful metadata request does not establish that
qualification. Do not substitute `xcb run` when application generation is
unavailable.

## Discover available accounts and models

```sh
xcb --json generate --capabilities
```

This reads local account/model metadata without refreshing a provider, connecting
an account or making an inference call. It does not initialize a missing state
directory. Existing installations open their database read-only without initialization or
migration; SQLite may maintain its normal reader coordination sidecars.
Only accounts with `available: true` are eligible. `connected` reports local
credential presence/shape; it does not promise current remote authentication.
`runtimeAdmitted` describes provider artifact admission, independently of
application qualification. `supported` reports whether this build has any
qualified application provider; a supported build can still have no available
account.

```json
{
  "version": 1,
  "supported": false,
  "zeroTools": true,
  "zeroHooks": true,
  "ephemeral": true,
  "limits": {
    "maxInputBytes": 1048576,
    "maxOutputBytes": 262144,
    "minTimeoutMs": 1000,
    "maxTimeoutMs": 120000
  },
  "accounts": []
}
```

Account rows contain `id`, `label`, `provider`, `enabled`, `busy`, `connected`,
`runtimeAdmitted`, `available`, `reason` and `models`. Models contain the exact
public `key`, `label` and `observedAtMs`. Keys match `xcb models` and accounts
match `xcb accounts`. Catalog observations expire after 24 hours. A reason is
`application_not_qualified`, `account_disabled`, `account_busy`, `not_connected`,
`runtime_unavailable`, `models_unavailable`, or `null` when ready.

A qualified account additionally carries `qualification` with `runtimeVersion`,
`runtimeDigest`, `evidenceDigest` and `expiresAt` (Unix milliseconds).
`runtimeDigest` identifies the exact XCB executable. The separately reviewed
evidence binds its provider pin, isolation controls and live application tests.
An application must reject missing, expired or mismatched evidence; neither
account sign-in nor caller JSON can issue it.

## Generate one response

Start the exact admitted XCB executable directly, write one UTF-8 JSON document
to stdin, close stdin and read its bounded stdout:

```json
{"version":1,"account":"a_selected_account","model":"claude/observed-model/observed-effort","prompt":"Return the requested application response.","timeoutMs":60000,"maxOutputBytes":65536}
```

Use `xcb --json generate`. All six fields are required and additional fields
are rejected. The entire input is limited to 1 MiB, including JSON framing.
`timeoutMs` is 1,000–120,000 and `maxOutputBytes` is 1–262,144. Prompts must be
nonempty UTF-8 without NUL. Account selection and the full observed model key
are exact; no implicit default or fallback is used.

Success is one JSON object and exit code zero:

```json
{"version":1,"status":"completed","requestId":"application_generated_id","account":"a_selected_account","model":"claude/observed-model/observed-effort","text":"application response","outcome":{"terminal":"completed","joined":true,"effects":"none"}}
```

XCB emits success only after native process-group, protocol and egress joins,
credential persistence and durable account lease settlement. `effects: none`
means no application tools or actions were performed; required XCB authentication
and custody maintenance still occurs. Validate `text` against your application's
own schema before using it. The provider is not claimed to enforce arbitrary JSON
schemas.

Failures use a nonzero exit code and a closed object with `version: 1`,
`status: failed`, `code`, and `requestId` when known. Codes are `invalid_request`,
`unavailable`, `busy`, `deadline`, `cancelled`, `provider_error`, `output_limit`
and `custody_unproven`. `joined: true` and `effects: none` appear only when
independently established. Failure responses never include generated text,
provider payloads, stderr, credentials or private paths.

For an execution failure, the host may retain one bounded private diagnostic per
account. Inspect it using the exact account and application request ID:

```sh
xcb --json application-diagnostic --account ACCOUNT_ID --request application_REQUEST_ID
```

This command is read-only: it does not initialize state, refresh an account,
read credentials or launch a provider. An absent, older, replaced or unreadable
record returns `unavailable`; it does not reconstruct details from past runs.
The existing version-one `generate` and qualification failure objects are
unchanged.

The diagnostic contains request/run/account identities, provider, timestamp,
a closed execution stage and category, and optional closed RPC operation,
numeric RPC code and protocol-check reason. For example, a Devin resource
refusal can retain `receive`, `quota_or_resource_limit`, `session_prompt` and
`-32011`; a model-selection mismatch retains `initialize`, `protocol` and
`model_changed`. Unknown protocol checks become `other`. No original error
string, prompt, reply, provider payload, stderr, credentials or path is stored.

Publication uses the existing account lease and replaces only that account's
previous diagnostic, with a 4 KiB limit and owner-only file permissions. It is
best effort: a diagnostic I/O failure never changes the execution result or
weakens cleanup. Preparation, initialization, prompt start, response decoding
and rejected output can produce records. Pre-admission refusals, cancellation,
deadlines and output-size limits need not produce one. A diagnostic is **not**
proof of process termination, account settlement, qualification, cost or an
entitlement; use the command's settlement fields and normal qualification gates.

## Cancellation and recovery

Send SIGINT or SIGTERM and wait for the command to finish its cleanup. XCB
continues joining processes and settling credential/account custody after the
inference deadline. A deadline is not a promise that cleanup finishes at the
same instant. Killing XCB, dropping its future or seeing its root process exit
does not prove that a provider has stopped. An uncertain outcome retains the
account's custody record and blocks new work; do not delete it or blindly retry.

Applications own their own durable request records, privacy controls, output
validation and external effects. Textbutler's recipient-bound grants, final
takeover checks and send journal remain necessary even when XCB has successfully
generated a response. XCB never sends messages for the application.

## Qualification and expiry

The host qualification command accepts a private directory of actual native
boundary and source/test evidence, then runs a fixed harmless challenge through
the same ephemeral executor:

```sh
xcb --json qualify-application --account <account-id> --model <full-model-key> --evidence /absolute/private/evidence-directory
```

The evidence must match the current executable, provider, platform and effective
application settings. The command accepts no caller prompt, tools or availability
override. It publishes a receipt only after the fixed response, process and
protocol joins, credential settlement and exclusive publication lease are
verified. Application `generate` cannot invoke this qualification path. Each invocation
qualifies one explicit model and atomically replaces that account's previous
application coverage; it does not accumulate other model qualifications.

Receipts expire at a fixed deadline no later than 24 hours after their evidence
collection began. Reads never extend it. Exact executable/provider changes and
explicit account credential replacement invalidate it immediately. Model
observations also expire after 24 hours; `xcb accounts refresh <account>` obtains
fresh provider metadata. Expired qualification requires new valid evidence and a
new fixed challenge. For one previously qualified Claude account/model on macOS,
the [explicit renewal helper](application-renewal.md) collects fresh evidence and
runs that challenge against a pinned deployment and account generation. It does
not extend the 24-hour lifetime or activate automatically. Unattended use requires
explicit binding and LaunchAgent installation, plus a coordinated first live
renewal whose actual result and final installed bytes have been verified.

Discovery emits models only when covered by that account's valid qualification.
The complete response is limited to 128 accounts, 64 qualified models per
account, 1,024 models total, and 2 MiB. An oversized inventory fails discovery
closed; it is never silently truncated. These cardinalities also keep the closed
version-one schema below 131,072 JSON tokens.

Trusted qualification tooling can read the executable's exact binding without
launching a provider or changing local state:

```sh
xcb --json qualify-application --inspect --account ACCOUNT_ID --model claude/sonnet/low
```

Inspection returns the XCB and provider versions and SHA-256 digests, OS,
architecture, effective policy/configuration digests, and selected account/model.
It does not grant qualification or extend a receipt.
