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
