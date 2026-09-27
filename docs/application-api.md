# Build applications with an AI subscription

`xcb --json generate` gives an application one model response per call from a
coding-agent subscription. xcb handles provider sign-in, the provider's
sandbox, and the provider process, and holds the account while the call runs.
The application sends a prompt of up to 1 MiB and receives untrusted text; its
own code decides which files, network requests, or messages that text can lead
to. [Textbutler](https://github.com/hraness/textbutler) is an example: it uses
separate classification and reply prompts and keeps contact memory, recipient
selection, review, and message delivery in its own code.

The application route is separate from `xcb run`. It creates no saved session,
reads no conversation history, and turns on no tools, hooks, plugins, judge,
continuation, or switching to another account. Private run records keep the
account, process, model, and timing without storing application prompts or
replies. Provider sign-in stays in xcb; applications never pass credentials.

For Devin ACP, the host instructions and application prompt are combined in one
text content block, separated by a blank line; there is no separate system-role
message. This tool-free route does not add the MCP workspace instructions used
by tool-enabled Devin sessions. The native decoder follows
[ACP extension semantics](https://agentclientprotocol.com/protocol/v1/extensibility):
unrecognized, size-limited, valid underscore-prefixed notifications without an `id`
are discarded
without events, replies, or changes to the session, model, tools, or account. Unknown
requests still receive a method-not-found response and trigger attention;
ordinary unknown notifications and invalid `session/update` messages still fail.
Recognized `_cognition.ai/compaction` notifications retain their closed-shape,
matching-session and active-turn checks before this extension fallback. Their
summary is discarded without changing output or tool state. This handling does
not approve a provider for application use.

An installation reports `supported: false` until the application path has
passed xcb's application checks for that xcb executable, provider, account, and
model. The record of those checks is called a qualification. A provider pin, a
configured account, or a successful metadata request doesn't create one. Do not
substitute `xcb run` when application generation is unavailable.

## Discover available accounts and models

```sh
xcb --json generate --capabilities
```

This reads local account/model metadata without refreshing a provider, connecting
an account or making an inference call. It does not initialize a missing state
directory. Existing installations open their database read-only without initialization or
migration; SQLite may maintain its normal reader coordination sidecars.
Use only accounts with `available: true`. `connected` reports that local
credentials exist and look valid; it doesn't promise the provider still accepts
them. `runtimeAdmitted` reports whether xcb supports the provider build,
separately from the application checks. `supported` reports whether this build
has any provider that passed the application checks; a supported build can
still have no available account.

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
    "maxTimeoutMs": 300000
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
`runtimeDigest` identifies the exact xcb executable. The separately reviewed
evidence binds its provider pin, isolation controls and live application tests.
An application must reject missing, expired or mismatched evidence; neither
account sign-in nor caller JSON can issue it.

## Generate one response

Start the xcb executable that passed the checks directly, write one UTF-8 JSON
document to stdin, close stdin, and read stdout:

```json
{"version":1,"account":"a_selected_account","model":"claude/observed-model/observed-effort","prompt":"Return the requested application response.","timeoutMs":60000,"maxOutputBytes":65536}
```

Use `xcb --json generate`. All six fields are required and additional fields
are rejected. The entire input is limited to 1 MiB, including JSON framing.
`timeoutMs` is 1,000–300,000 and `maxOutputBytes` is 1–262,144. Prompts must be
nonempty UTF-8 without NUL. Account selection and the full observed model key
are exact; no implicit default or fallback is used.

Success is one JSON object and exit code zero:

```json
{"version":1,"status":"completed","requestId":"application_generated_id","account":"a_selected_account","model":"claude/observed-model/observed-effort","text":"application response","outcome":{"terminal":"completed","joined":true,"effects":"none"}}
```

xcb reports success only after the provider's processes, protocol connection,
and network bridge have exited, any refreshed credentials are saved, and the
account is released. `effects: none` means no application tools or actions ran;
xcb's own sign-in and account bookkeeping still happen. Validate `text` against your application's
own schema before using it. The provider is not claimed to enforce arbitrary JSON
schemas.

Failures use a nonzero exit code and a fixed-shape object with `version: 1`,
`status: failed`, `code`, and `requestId` when known. Codes are `invalid_request`,
`unavailable`, `busy`, `deadline`, `cancelled`, `provider_error`, `output_limit`,
and `custody_unproven` (xcb couldn't confirm the provider stopped, so it keeps
the account held). `joined: true` and `effects: none` appear only when xcb
confirmed them. Failure responses never include generated text,
provider payloads, stderr, credentials or private paths.

For an execution failure, the host may keep one small private diagnostic per
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

Writing a diagnostic replaces only that account's previous one, with a 4 KiB
limit and owner-only file permissions. It is
best effort: a diagnostic I/O failure never changes the execution result or
weakens cleanup. Preparation, initialization, prompt start, response decoding
and rejected output can produce records. Pre-admission refusals, cancellation,
deadlines, and output-size limits need not produce one. A diagnostic doesn't
prove that the process stopped, the account was released, the checks passed, or
anything about cost or entitlement; use the command's outcome fields and the
normal application checks.

## Cancellation and recovery

Send SIGINT or SIGTERM and wait for the command to finish its cleanup. After
the deadline, xcb keeps waiting for the provider's processes to exit and saves
credentials before releasing the account, so cleanup can outlast the deadline.
Killing xcb or seeing its main process exit doesn't prove that a provider has
stopped. When the outcome is uncertain, xcb keeps the account held and blocks new
work on it; don't delete its records or blindly retry.

Applications own their own durable request records, privacy controls, output
validation and external effects. Textbutler's recipient-bound grants, final
takeover checks and send journal remain necessary even when xcb has successfully
generated a response. xcb never sends messages for the application.

## Application checks and expiry

The host check command takes a private directory of sandbox and source/test
evidence collected on the host, then runs a fixed harmless challenge through the
same path as `generate`:

```sh
xcb --json qualify-application --account <account-id> --model <full-model-key> --evidence /absolute/private/evidence-directory
```

The evidence must match the current executable, provider, platform and effective
application settings. The command accepts no caller prompt, tools or availability
override. It saves a qualification only after the fixed response is correct,
the provider's processes and connection have exited, credentials are saved, and
it holds the account exclusively. `generate` can't run this path. Each run covers
one model and replaces that account's previous coverage; it doesn't add to other
models' qualifications.

Qualifications expire at a fixed deadline no later than 24 hours after evidence
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
account, 1,024 models total, and 2 MiB. An oversized inventory makes discovery
fail rather than be silently truncated. These cardinalities also keep the closed
version-one schema below 131,072 JSON tokens.

Trusted qualification tooling can read the executable's exact binding without
launching a provider or changing local state:

```sh
xcb --json qualify-application --inspect --account ACCOUNT_ID --model claude/sonnet/low
```

Inspection returns the xcb and provider versions and SHA-256 digests, OS,
architecture, effective policy/configuration digests, and selected account/model.
It doesn't pass the checks or extend a qualification.
