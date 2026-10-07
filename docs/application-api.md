# Build applications with an AI subscription

`xcb --json generate` gives an application one model response per call from a
coding-agent subscription. xcb handles provider sign-in, the provider's
sandbox, and the provider process, and holds the account while the call runs.
The application sends a prompt of up to 1 MiB and receives untrusted text; its
own code decides which files, network requests, or messages that text can lead
to. [TextButler](https://github.com/hraness/textbutler) is an example: it uses
separate classification and reply prompts and keeps contact memory, recipient
selection, review, and message delivery in its own code.

The application route is separate from `xcb run`. It creates no saved session,
reads no conversation history, and turns on no tools, hooks, plugins, judge,
continuation, or switching to another account. Private run records keep the
account, process, model, and timing without storing application prompts or
replies. Provider sign-in stays in xcb; applications never pass credentials.

Once you sign in an account, apps can use it with no extra command. The first
`generate` for an account and model checks it automatically: xcb sends one
fixed harmless prompt through the same no-tools path and checks the reply. Later
calls skip the check until xcb, the provider build, the app settings, or the
account's sign-in changes. An installation reports `supported: false` when no
provider build on this computer can serve apps, for example when xcb can't
confirm the provider's sandbox here. Do not substitute `xcb run` when
application generation is unavailable.

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
`runtimeAdmitted`, `available`, `reason`, `admission` and `models`. Models
contain the exact public `key`, `label`, `observedAtMs` and `admission`. Keys
match `xcb models` and accounts match `xcb accounts`.

`admission` says how far a model has been checked for this exact xcb, provider
build, settings and sign-in:

| Value | Meaning |
| --- | --- |
| `pending` | Not checked yet. It is usable: the next `generate` checks it first, so that call takes longer (up to 60 seconds more). |
| `admitted` | The automatic check passed. |
| `qualified` | A strict manual qualification covers it (see below). |

The account-level `admission` is the best value among its models, or `null`
whenever the account is unavailable (`reason` says why). On an unavailable
account, a model's `admission` is `null` too unless a strict qualification
covers it, so `pending` never appears beside a reason. A `pending` model must
have been seen in the last 24 hours; `xcb accounts refresh <account>` obtains fresh
provider metadata. Once admitted or qualified, a model stays listed without
further catalog refreshes.

A reason is `application_disabled` (the owner turned app access off),
`account_disabled`, `authentication_required`, `account_busy`, `not_connected`,
`runtime_unavailable`, `sandbox_unproven` (xcb couldn't confirm the provider's
sandbox on this computer), `admission_failed` (the automatic check failed for
every listed model), `models_unavailable`, or `null` when ready. `busy` is true
while the account has any unfinished run; `account_busy` means its runs reached
the configured `max_runs_per_account` limit, so the account cannot take another
task right now. Version 0.19 and earlier also reported
`application_not_qualified`; newer versions don't, so treat unknown reasons as
unavailable.

A manually qualified account additionally carries `qualification` with `runtimeVersion`,
`runtimeDigest`, `evidenceDigest` and `expiresAt`, which is always `null`:
qualification has no time limit. `runtimeDigest` identifies the exact xcb
executable. The separately reviewed evidence binds its provider pin, isolation
controls and live application tests. An application must reject missing or
mismatched evidence; neither
account sign-in nor caller JSON can issue it.

## Generate one response

Start the xcb executable directly, write one UTF-8 JSON document to stdin, close
stdin, and read stdout:

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
numeric RPC code and protocol-check reason. For example, a resource
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
validation and external effects. TextButler's recipient-bound grants, final
takeover checks and send journal remain necessary even when xcb has successfully
generated a response. xcb never sends messages for the application.

## Application checks

### Automatic check on first use

The first `generate` for an account and model runs the check before your
request:

1. xcb confirms the provider's sandbox works on this computer. On macOS it runs
   a short local test of the provider's sandbox profile; on Linux it uses the
   receipt from `xcb doctor --qualify-sandbox`. This needs no account and runs
   once per xcb build and provider. If it fails, apps can't use that provider
   here (`sandbox_unproven`), and xcb tries again 15 minutes later.
2. xcb holds the account and sends one fixed harmless prompt through the same
   no-tools path `generate` uses, then checks the reply exactly. The provider's
   processes must exit and credentials must be saved before the result counts.
3. xcb records the result, then serves your request. Your `timeoutMs` starts
   after the check, which has its own 60-second limit.

Only one check runs per account at a time. Other calls for that account wait
for it, then return `busy` if it's still running. If the model answers with the
wrong text, or more than the check allows, the model stays unavailable
(`admission_failed`, and `generate` returns `unavailable`) for 15 minutes or
until something it covers changes. A provider error (such as a rate limit), a
deadline, cancellation or local failure records nothing, so the next call checks
again. Each xcb build keeps its own results, so two xcb executables sharing one
state folder don't undo each other's checks.

A check covers one account and model for the exact xcb executable, provider
build, platform, application policy and configuration, and the account's
sign-in. When any of these changes, for example after an xcb or provider
update, the next `generate` checks again automatically. The record holds those
identities, the result and a time. It never holds a prompt, the check's reply,
or your text.

### Turn app access off

Apps can use every signed-in account until you turn access off:

```sh
xcb application disable                  # every account
xcb application disable --account ID     # one account
xcb application enable [--account ID]
xcb application status
```

While access is off, `--capabilities` reports `application_disabled` and
`generate` returns `unavailable` without starting a provider. This also stops
accounts that were already checked or qualified. If xcb can't read the setting,
it treats access as off.

### Strict manual qualification (optional)

The strict manual qualification remains available as a stronger record for an
exact deployment. It is optional: `generate` doesn't require it. It takes a
private directory of sandbox and source/test evidence collected on the host,
then runs the same fixed challenge:

```sh
xcb --json qualify-application --account <account-id> --model <full-model-key> --evidence /absolute/private/evidence-directory
```

The evidence must match the current executable, provider, platform and effective
application settings. The command accepts no caller prompt, tools or availability
override. It saves a qualification only after the fixed response is correct,
the provider's processes and connection have exited, credentials are saved, and
it holds the account exclusively. Each run covers one model and replaces that
account's previous coverage; it doesn't add to other models' qualifications.
A qualified model reports `admission: "qualified"` and needs no automatic check.

A qualification has no time limit. It ends when anything it covers changes: the
xcb executable, the provider build, the platform, the application policy or
configuration, or the account's sign-in after an explicit credential
replacement. Collection itself must finish within 24 hours of its first
observation, and the model must have been seen in the last 24 hours when
qualifying. The [renewal helper](application-renewal.md) remains available for
requalifying a Claude account/model on macOS after such a change.

### What protects you

Earlier versions required the strict qualification before any app call. It
re-proved, for every account and model, facts about the xcb build: that the
workspace tests passed, that a frozen source build matched the running binary,
and, for Codex and Devin, a separately produced provider-boundary receipt. Those
are now proved once, where they belong:

- **Build facts are proved with the build.** A release binary comes from a
  `main` commit whose required checks ran the workspace tests, and carries a
  build-provenance attestation bound to its digest. A build from source carries
  only what its builder checked; xcb doesn't verify that attestation at run
  time, so the runtime controls below are what protect every build. Provider builds run only
  when xcb's source or its reviewed catalog names their exact digest, and that
  admission already checks tools, configuration isolation and file access.
- **Host facts are proved automatically.** The sandbox check above runs without
  credentials, once per xcb build and provider. If it can't pass, apps can't
  use that provider; nothing falls back to an unsandboxed run.
- **Account facts are proved automatically.** The fixed challenge confirms the
  account, model and provider answer through the exact no-tools path, and is
  repeated whenever anything it covers changes.

What protects you on every call is unchanged and enforced at run time, not by a
record: no tools, hooks or plugins; the provider's sandbox and isolated
configuration; credentials that stay in xcb; exact provider-build checks before
launch; exclusive account custody with `custody_unproven` holding the account
when xcb can't confirm the provider stopped; and failures that never include
payloads. The owner switch above turns access off at any time.

Discovery emits models only when they are qualified, admitted, or pending for
that account. The complete response is limited to 128 accounts, 64 models per
account, 1,024 models total, and 2 MiB. Qualified and admitted models count
first; pending models fill the remaining room in catalog order, and any beyond
it are left out until earlier ones are admitted. Otherwise an oversized
inventory makes discovery fail rather than be silently truncated. These cardinalities also keep the closed
version-one schema below 131,072 JSON tokens.

Trusted qualification tooling can read the executable's exact binding without
launching a provider or changing local state:

```sh
xcb --json qualify-application --inspect --account ACCOUNT_ID --model claude/sonnet/low
```

Inspection returns the xcb and provider versions and SHA-256 digests, OS,
architecture, effective policy/configuration digests, and selected account/model.
It doesn't pass the checks or extend a qualification.
