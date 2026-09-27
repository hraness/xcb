# TypeScript SDK

The TypeScript SDK's `createSubscriptionRouter` runs one task at a time on the
account and model your app names, and holds that account until the provider
process has exited. Your app names both. To let xcb choose the account and
model, call [`xcb --json route`](route.md) instead.

## Install

The SDK is the `@hraness/xcb` package on npm. It runs on Node 22.13 or later
and Bun 1.3.14 or later:

```sh
npm install @hraness/xcb
# or
bun add @hraness/xcb
```

The package also installs the `xcb-compat` command, which is separate from the
native `xcb`. To build the package from source, see the
[compatibility reference](compatibility.md#build-from-source).

## A complete example

This program runs as is. It uses a stand-in adapter that starts no provider
and echoes the prompt, so you can watch the router hold the account during
the task and release it after. Save it as `router-demo.ts` in the project
where you installed the SDK:

```ts
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  createCapabilityBroker,
  createCapabilityProfile,
  createSubscriptionRouter,
  openAccountDatabase,
  SqliteAccountLeases,
  type AgentTaskAdapter,
  type AgentTaskExecutionRequest,
} from "@hraness/xcb";

const sha256 = (text: string) => createHash("sha256").update(text).digest("hex");

// 1. The account store. A real host keeps this file in its own private
//    folder, outside every project folder, and shares it between processes.
const db = await openAccountDatabase(join(tmpdir(), "xcb-router-demo.sqlite"));
const leases = new SqliteAccountLeases(db);

// 2. The tools a model may call during the task. This demo offers none.
const profile = createCapabilityProfile({ id: "demo.no-tools", version: 1, tools: [] });
const profileId = { id: profile.id, version: profile.version, digest: profile.digest };

// 3. A stand-in adapter. It starts no provider and answers with an echo, so
//    you can watch the router hold and release the account.
const route = { id: "demo-claude", provider: "claude", authentication: "subscription" } as const;
const runtime = { version: "demo-1", digest: sha256("demo-1") };
const binding = (request: AgentTaskExecutionRequest) => ({
  route: request.route, accountId: request.accountId, workspaceId: request.workspaceId,
  runId: request.runId, profile: request.profile, model: request.model,
  runtime: request.runtime, accountLease: request.accountLease,
});
const demoAdapter: AgentTaskAdapter = {
  route,
  runtime,
  qualification: {
    status: "qualified", route, profile: profileId,
    runtimeVersion: runtime.version, runtimeDigest: runtime.digest,
    evidenceDigest: sha256("demo evidence"), expiresAt: Date.now() + 60 * 60 * 1000,
    controls: {
      noCommandTools: true, exactToolInventory: true, workspaceReadIsolation: true,
      workspaceWriteIsolation: true, isolatedConfiguration: true,
      authOutsideWorkspace: true, hostBrokerOnly: true,
    },
  },
  async run(request) {
    const holder = leases.inspect(request.route.provider, request.accountId)?.owner;
    return {
      ...binding(request),
      output: `echo: ${request.prompt} (account held by ${holder})`,
      usage: { inputTokens: null, outputTokens: null, totalTokens: null, costUsd: null },
      outcome: { status: "completed", code: null },
    };
  },
  async stop(request) {
    return {
      ...binding(request),
      processStopped: true, controllersStopped: true, joined: true,
      stoppedAtUnixMs: Date.now(), proofDigest: sha256(`stopped ${request.runId}`),
    };
  },
};

// 4. The router: your account store plus the adapters you trust.
const router = createSubscriptionRouter({ leases, adapters: [demoAdapter] });

// 5. One task. Your app names the account and the model.
const broker = createCapabilityBroker({
  profile, workspaceId: "demo-project", runId: "run-1", isActive: () => true,
});
const result = await router.run({
  provider: "claude",
  accountId: "work-claude",
  profile: profileId,
  model: { id: "claude-sonnet", reasoningEffort: "low", serviceTier: null },
  purpose: "respond",
  prompt: "Summarize the open pull requests.",
  limits: { maxRunMs: 60_000, maxCleanupMs: 10_000, maxOutputBytes: 65_536 },
}, broker);

console.log(result.outcome.status); // completed
console.log(result.output);         // echo: … (account held by run-1)
console.log(result.custody);        // released
console.log(leases.inspect("claude", "work-claude")); // null: free for the next task
db.close();
```

Run it with `node router-demo.ts` (Node 24 or later runs TypeScript directly)
or `bun router-demo.ts`. It prints:

```text
completed
echo: Summarize the open pull requests. (account held by run-1)
released
null
```

## What each part does

- **Account store:** `SqliteAccountLeases` records which task holds each
  account. While one task holds an account, another `run` on it fails with
  `ACCOUNT_BUSY_OR_RECOVERY_REQUIRED`.
- **Capability profile and broker:** the tools a model may call, bound to one
  workspace and run. `isActive` lets your app revoke them mid-task.
- **Adapter:** starts the provider, runs the turn, and proves in `stop` that
  the provider's processes have exited. The router releases the account only
  after `stop` returns that proof.
- **Request:** the account, model, prompt, and limits. `maxRunMs` plus
  `maxCleanupMs` is the whole deadline, at most one hour; pass `signal` to
  cancel sooner.

## Use a real provider

Replace the stand-in with an adapter that launches a provider:
`createClaudeTaskAdapter` (Claude Code), `createClaudeApiAdapter` (the Claude
API), or `createCodexTaskAdapter` and `createCodexManagedTaskAdapter` (Codex).
The Devin adapter is present but not enabled for tasks.

A real adapter runs only with a qualification record from your host: evidence
that its exact provider build runs with the expected tools, configuration, and
file access. The SDK checks that record on every task. It doesn't create the
record, and it doesn't discover credentials. The
[compatibility reference](compatibility.md#embedding-the-subscription-router)
documents each adapter's options, and [MANAGED-CODEX.md](../MANAGED-CODEX.md)
covers the Codex host requirements.

## Errors

The router throws an `Error` whose message is a code:

- `ROUTER_ROUTE_REQUIRED`, `ROUTER_ROUTE_UNAVAILABLE`, or
  `ROUTER_ROUTE_AMBIGUOUS`: name a registered provider, or pass the exact
  `route` when several adapters serve one provider.
- `ACCOUNT_BUSY_OR_RECOVERY_REQUIRED`: another task holds the account, or a
  stopped task's hold wasn't released.
- `TASK_ADAPTER_UNQUALIFIED` or `TASK_QUALIFICATION_MISMATCH`: the adapter's
  qualification record is missing, expired, or for another build.
- `TASK_CUSTODY_UNPROVEN`: the adapter couldn't prove the provider stopped, so
  the account stays held. Recover it only with independent proof that the
  process exited.
