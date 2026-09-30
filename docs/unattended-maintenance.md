# Excalibur (xcb) unattended maintenance

The host maintenance runner samples memory, disk space, and supervisor health every minute. An optional Codex review investigates incidents and performs an hourly check in the same conversation. Sampling continues during reviews. The Python runner never deletes caches or cancels xcb tasks itself.

This is a trusted host operator, separate from xcb's sandboxed coding workers. It uses the host's Codex configuration, rules, authentication, and automatic approval review. The maintenance prompt requires the installed `local-efficiency` skill and each repository's scheduler before cleanup or process recovery. Permission denials remain in effect.

## Prepare a machine

Use a stable checkout and an xcb build that supports `xcb --json resources --workspace ABSOLUTE_PATH` and the supervisor health fields in `xcb --json service status`. Run the local-efficiency doctor first and verify the intended workspace permission boundary, `on-request` approval policy, and `auto_review` reviewer. The runner does not change these settings.

Create a private directory for the configuration and supply absolute paths for the tools, skill, and workspace:

```sh
mkdir -m 700 /ABSOLUTE/maintenance-config
python3 scripts/unattended-maintenance.py init \
  --config /ABSOLUTE/maintenance-config/config.json \
  --state-dir /ABSOLUTE/maintenance-state \
  --xcb /ABSOLUTE/xcb \
  --codex /ABSOLUTE/codex \
  --scheduler /ABSOLUTE/host-run \
  --bun /ABSOLUTE/bun \
  --skill /ABSOLUTE/local-efficiency/SKILL.md \
  --workspace /ABSOLUTE/trusted-workspace \
  --model gpt-6.1-sol --reasoning high
```

`init` creates the state directory with owner-only permissions. It records the executable paths, resolved targets, and SHA-256 hashes, including Python and the runner. Each command checks the files it needs: sampling checks Python, the runner, and xcb; sending a heartbeat checks Python and the runner; model reviews check every pinned tool. A changed Codex, Bun, scheduler, or skill blocks model reviews and records `review_tool_binding_changed`, while resource sampling and external heartbeats continue with an attention state. `status` and `plan` identify affected tools using fixed names only. Changed core files stop their dependent commands. No command silently accepts an upgrade. After reviewing the change, run `rebind --config /ABSOLUTE/maintenance-config/config.json`. Rebinding preserves existing run state and disables reviews until they are explicitly re-enabled. Preserve the existing state directory during upgrades; its uncertain-run records must not be reset. Bun's directory enters the controlled PATH because `host-run` uses Bun.

The configuration starts with reviews disabled. These commands inspect the machine and the exact proposed prompt without using a model:

```sh
python3 scripts/unattended-maintenance.py sample --config /ABSOLUTE/maintenance-config/config.json
python3 scripts/unattended-maintenance.py status --config /ABSOLUTE/maintenance-config/config.json
python3 scripts/unattended-maintenance.py plan --config /ABSOLUTE/maintenance-config/config.json
```

The prompt and saved samples contain selected numeric metrics and fixed incident codes. Native error messages, task text, process arguments, environments, and provider transcripts are excluded. Configuration paths are operator-supplied trusted input.

## Enable recurring checks

Enable model reviews after inspecting `plan` and testing the exact Codex build, account, model, and automatic approval configuration on the machine:

```sh
python3 scripts/unattended-maintenance.py enable --config /ABSOLUTE/maintenance-config/config.json
python3 scripts/unattended-maintenance.py review --config /ABSOLUTE/maintenance-config/config.json
```

The review uses `codex --no-daemon exec --json`, with the selected model and reasoning effort. Later reviews resume the exact recorded session UUID. It neither resumes an unrelated latest session nor changes sandbox and approval settings. `--no-daemon` keeps the run independent of the user's shared Codex app daemon. Saved CLI authentication remains in its existing home directory; do not copy credentials into the maintenance configuration.

Prepare the two LaunchAgent files in a private staging directory:

```sh
mkdir -m 700 /ABSOLUTE/maintenance-plists
python3 scripts/unattended-maintenance.py install \
  --config /ABSOLUTE/maintenance-config/config.json \
  --output-dir /ABSOLUTE/maintenance-plists
```

`install` only writes the plists. It does not copy them into `~/Library/LaunchAgents`, load them, change login settings, or run a model. Each plist uses `RunAtLoad` and a 60-second interval. One invokes `sample`; the other invokes `review`, which returns immediately when no review is due. After inspecting the files, install and bootstrap them through the usual user LaunchAgent procedure. They start after the user logs in. A pre-login FileVault unlock remains a separate requirement.

Use `disable` with the configuration path to pause future model reviews while sampling continues. An existing review holds its lock until it exits; disabling does not kill a review in progress.

## Cadence and limits

| Setting | Default |
| --- | --- |
| Sample interval | 60 seconds |
| Periodic review | 1 hour |
| Minimum time between reviews | 30 minutes |
| Maximum review starts in a rolling day | 36 |
| Review deadline | 10 minutes |
| Each native status command | 15 seconds, 256 KiB output |
| Review output | 4 MiB, including discarded stderr |
| Sample history | Latest 1,440 samples, at most 24 hours |
| Review history | Latest 96 run summaries |
| Disk warning / critical | 60 GiB / 20 GiB free |
| Swap growth warning | At least 2 GiB growth over 10–15 minutes |

A critical memory sample triggers an incident immediately. Warning memory pressure requires three samples covering at least 100 seconds. The runner also reports a paused resource guard, missing telemetry, stale supervisor heartbeat, and a disabled watchdog. A stopped supervisor with no running process is normal when idle.

The native resource guard decides whether new work can start. These review thresholds do not replace a repository's disk reserve or its estimate of the next job's peak allocation. Healthy hourly checks consume about 720 reviews over 30 days; incident reviews can increase that within the daily limit. The runner does not enforce a dollar or token budget within a single model turn. The deadline and daily cap bound frequency and duration. Existing Codex session storage follows Codex's own retention rules and is protected from this janitor.

Persistent file locks prevent overlapping samplers or reviews. Sampling has a separate lock so a slow review cannot stop resource observations. State is atomically replaced and synced. Launchd output goes to `/dev/null`; `status` reads the bounded local records. There are no rotating raw transcripts or general-purpose notification hooks in the runner. An optional fixed-schema heartbeat is described below.

## Optional external heartbeat

The runner can send an HTTPS heartbeat to the xcb Convex backend every five minutes. This remains disabled unless an explicit receiver URL and credential file are configured. The public status page can detect a missing laptop from the time of its last accepted heartbeat, even when the laptop cannot send an alert.

Provision a separate 32-byte random bearer credential for each machine. The private token file must contain exactly 64 lowercase hexadecimal characters, with an optional trailing newline, and have mode `0600`. The receiver stores the token's SHA-256 digest. Keep the plaintext token out of shell arguments, environment variables, model prompts, logs, and source control.

Supply `--heartbeat-url` and `--heartbeat-token-file` during `init`, or configure an existing runner:

```sh
python3 scripts/unattended-maintenance.py heartbeat-configure \
  --config /ABSOLUTE/maintenance-config/config.json \
  --heartbeat-url https://DEPLOYMENT.convex.site/host-status/heartbeat \
  --heartbeat-token-file /ABSOLUTE/private-heartbeat-token
```

Replace `DEPLOYMENT` with the receiver's lowercase deployment label. The URL must match this HTTPS host and path format exactly; user information, ports, queries, fragments, redirects, and proxy settings are rejected. `heartbeat-disable --config /ABSOLUTE/maintenance-config/config.json` stops future sends. It preserves the sequence counter for a later restart or token rotation.

After saving a sample and releasing its lock, the sampler starts a separate pinned Python process to send the request. An eight-second wall-clock deadline also bounds DNS resolution; the HTTP socket timeout is five seconds. A separate persistent lock prevents overlapping senders. Network waits hold no scheduler lease, and a failed request cannot erase the completed sample.

The request contains only these fields:

```json
{"version":1,"sequence":1,"health":"ok","sampleAgeSeconds":0}
```

`health` is `ok`, `degraded`, or `unknown`. Reporting `ok` requires healthy resources and a fresh, running supervisor confirmed under watchdog supervision. Missing measurements or supervision flags, and a stopped supervisor, produce `unknown`. A running supervisor with a stale heartbeat or without watchdog supervision produces `degraded`. An idle stopped supervisor still does not trigger an incident review. Hostnames, process IDs, task content, paths, usernames, raw errors, and resource values are excluded. The receiver identifies the device from its bearer token and uses its own clock for freshness. Public device aliases should remain generic, such as `laptop-1` and `laptop-2`.

The runner persists a strictly increasing sequence before each attempt. A crash consumes that sequence. Only the newest sample, at most 180 seconds old, can be sent; there is no backlog or replay queue. Preserve `heartbeat.json` across upgrades. A successful HTTP `204` response records success. `401`, `409`, `429`, and `503` become fixed local status codes; response bodies are discarded. The receiver rejects repeated or lower sequences and accepted writes less than 60 seconds apart without refreshing its last-seen timestamp.

Failed requests back off for 5, 10, 20, then at most 30 minutes. A public page can therefore show the machine as offline after 15 minutes during an extended outage or backoff; it must not claim that the laptop is physically powered off. Two devices sending every five minutes produce approximately 17,280 writes over 30 days. The backend retains the latest device record; no cloud polling job is required to calculate its age.

`status` includes the sequence, last success, next attempt, and fixed outcome code. It exposes neither the token nor its file path. The maintenance model receives neither credential and is instructed to use sampled telemetry without reading the runner configuration. Heartbeat provisioning does not enable Slack, email, or messaging integrations.

## Uncertain reviews

The runner saves a unique run ID, configuration digest, prior session ID, and start time before launching Codex. A crash, timeout, malformed response, changed thread identity, or incomplete exit leaves this intent in place. Later reviews stop with `uncertain_previous_run`; sampling continues. The runner never treats an elapsed deadline or a missing PID as proof that the run stopped.

The deadline sends termination only to the runner's newly created process group. Any uncertain outcome remains blocked, including a successful CLI exit with surviving processes in that group. This does not authorize killing xcb workloads or other Codex sessions.

Recovery is an explicit host-operator action. Inspect the exact run and independently verify that its processes exited. Retain that evidence in a private JSON file with these fields:

```json
{
  "version": 1,
  "run_id": "EXACT_PENDING_RUN_ID_FROM_STATUS",
  "config_sha256": "EXACT_PENDING_CONFIGURATION_DIGEST",
  "operator": "IDENTITY_OF_REVIEWING_OPERATOR",
  "checked_at_s": 0,
  "all_processes_exited": true,
  "process_exit_evidence": "Describe independent evidence for the exact invocation and all its processes.",
  "outcome": "completed",
  "session_id": "EXACT_COMPLETED_CODEX_THREAD_UUID"
}
```

Set `checked_at_s` to the time the operator completed the check, as Unix seconds. `reconcile` accepts evidence at most one hour old, verifies the exact run and configuration identity, and records its digest:

```sh
python3 scripts/unattended-maintenance.py reconcile \
  --config /ABSOLUTE/maintenance-config/config.json \
  --evidence /ABSOLUTE/private-reviewed-exit-evidence.json
```

This file is an operator attestation, not a process-exit detector. Use `outcome: "abandoned"` when the run ended without a confirmed completed turn; the last confirmed session remains selected. Keep the evidence file. Do not remove lock files, edit away a pending marker, or create a fresh empty state directory to bypass uncertainty.

## Allowed remediation

The review prompt permits diagnosis, supported messages to affected agents, and xcb task cancellation only when a supported command checks the task's current revision. It forbids raw workload PID killing, account release after an uncertain exit, invented messaging commands, and changes to security or scheduling settings.

Cleanup follows the exact installed local-efficiency skill. Every candidate requires fresh proof of its exact location, ownership, reproducibility, absence of live users or open handles, and absence of protected state. A report-only janitor's candidate list supplies no deletion proof. Dirty or unmerged worktrees, source, credentials, databases, sessions, provider evidence, FIFOs, and ambiguous data stay protected. Actions run under the owning scheduler, and the reviewer measures settled physical free space after each cleanup.

## Before leaving the machines unattended

Run a multi-day soak test with real workloads. Check warning and recovery behavior, an intentionally failed status probe, a blocked or expired model login, a restarted sampler, a timed-out review, and a completed review that resumes its original thread. Check that review output and sample history remain within their limits. Synthetic unit tests cover control flow; they do not qualify a live model, authentication, automatic review, launchd registration, cleanup, or reboot recovery.

A second machine needs its own configuration, tools, account check, and soak test. Configure and test the optional external heartbeat receiver to detect a missing laptop or home network outage. An additional independent observer can detect an outage of the status service itself. This runner sends no Slack or email posts. Verify the complete reboot sequence from outside the home network: reachable network, FileVault unlock, user login, xcb startup, and resumed maintenance. Also verify sleep prevention and an independent backup/restore procedure.

The CLI invocation follows the [Codex non-interactive documentation](https://learn.chatgpt.com/docs/non-interactive-mode) and was checked against the installed CLI's `exec`, `exec resume`, and top-level help. Future CLI updates require rebinding and repeating the local tests.
