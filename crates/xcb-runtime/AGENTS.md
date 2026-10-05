# Native worker command boundary

- `workspace_exec` is the offline Lima replay: no host credentials, no network,
  filtered read-only Git, revision-checked publication. Never silently route a
  failed or unavailable replay to a host process.
- `workspace_host_exec` is a separate, explicitly host-admitted managed Codex
  tool. It runs in the real worktree with ordinary network and Git writes.
  `XCB_HOST_CREDENTIALS_TASK` must equal the persisted managed task ID of the
  current Codex session; direct sessions, other tasks and other providers must
  fail closed. This host-owned policy is not set by worker text or project files.
- Acquire `gh auth token`, Git author identity and SSH agent socket on the host
  only at execution time. Keep credentials out of argv, logs, receipts and
  provider account homes; use bounded private env-file staging and zeroized
  buffers. Scrub returned output before a tool result is recorded.
- A host command may change local Git and remote state before it exits. An
  interrupted, failed or unproven command must retain uncertain effects and
  account custody; never infer a retry from a timeout or missing response.
  Preserve the existing provider qualification, run-owner and account gates.
- `workspace_native_exec` is the separately granted macOS command bridge for
  supported providers. It requires current OS and provider checks for the exact
  executable, a matching workspace/provider grant, and a persisted native task
  requirement. It never substitutes offline replay or legacy host commands.
- Native GitHub checks must cover the login-service environment as well as the
  interactive CLI. Trusted credential helper lookup completes system-only PATH
  with standard absolute toolchain directories; worker command PATH remains
  limited to its explicitly granted roots. Never fetch or print live credentials
  merely to diagnose executable discovery.
- Verify actual native tool results, process/effect facts, and workspace changes
  for managed smoke tests. A completed task or verified task journal alone does
  not establish that its requested command ran successfully.
- When Claude inference usage is unavailable, `xcb doctor --provider claude`
  checks the supported build and `xcb accounts refresh <existing-account>`
  performs startup/model-catalog and usage-metadata queries without submitting
  a coding prompt. This is preparation only: unknown quota is not available
  quota, and metadata success does not clear an organization access refusal.
  `xcb native verify --provider claude` does submit a model prompt; defer it
  until authorized subscription access and inference usage are available.
- Account-specific Claude checks must use `xcb run --account <id>` and an
  observed Auto-compatible model such as Sonnet. Haiku does not support Auto
  mode and can fail the effective permission assertion before inference;
  never relax that assertion to make a sign-in check pass. The task-branch
  native verifier accepts `--account` and must retain it through routing;
  provider-wide verification alone cannot attest a particular new account.
- `xcb native describe --account <id>` performs quota-free startup and usage
  queries through the existing supervised probe. Project only bounded model,
  account-display, command-name and agent-name fields; never expose raw
  initialization, credentials, credential-source fields or agent prompts.
  Discovery is observational and does not authorize commands or delegation.
- `xcb native inspect --session <id>` reads bounded local session, run,
  command-custody, capability-process and tool-effect receipts only. It must
  not attach to or start a provider process, and a missing record is never
  evidence that a provider effect completed.
- `xcb native status` reads one bounded local account/session/run/tool
  snapshot for exact filters, JSON and JSONL output. It starts or attaches no
  provider, refreshes no credential or quota data, submits no prompt, and
  makes no recovery decision. Use `--session-state`; the global `--state`
  remains the state-root option. Session titles and transcript-derived prompt
  text stay out of the projection.
- Claude cooperative interruption is one-shot and only for an active turn.
  It does not replace process-group join, tool-effect settlement or retained
  account custody. Session/MCP/settings mutators require separate host-owned
  contracts and inventory revalidation; never add raw SDK pass-throughs.
- Provider method coverage is task-adapter accounting, not an activation grant.
  Keep `provider-methods.json` matched to the generated, digest-checked Codex
  schema and pinned Claude Query interface. Every unsupported Codex client
  control must fail before sending a request; requests and sensitive notices
  cannot turn an inventory entry into execution authority.
- Codex `native describe` is observational: no thread, prompt, or reset-credit
  consumption. Preserve the separate existing quota-refresh policy. Inspection
  uses an exact-account probe lease and must prove process/bridge cleanup.
- Claude `describe --runtime-status` targets its own metadata connection, not
  another running task. Request only summary context; return counters and
  bounded server status, never memory paths, server URLs/config/env/errors,
  tool descriptions, or raw payloads. Correlate exact request IDs, reject
  executable traffic, and retain custody on unproven cleanup.
