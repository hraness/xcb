/**
 * The SDK quickstart example, rendered on /docs/sdk and repeated verbatim in
 * docs/sdk.md; a test keeps the two identical. It runs as is against the
 * release archive under Node 24 and Bun 1.3.14.
 */
export const sdkExample = `import { createHash } from "node:crypto";
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
      output: \`echo: \${request.prompt} (account held by \${holder})\`,
      usage: { inputTokens: null, outputTokens: null, totalTokens: null, costUsd: null },
      outcome: { status: "completed", code: null },
    };
  },
  async stop(request) {
    return {
      ...binding(request),
      processStopped: true, controllersStopped: true, joined: true,
      stoppedAtUnixMs: Date.now(), proofDigest: sha256(\`stopped \${request.runId}\`),
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
db.close();`;

export const sdkExampleOutput = `completed
echo: Summarize the open pull requests. (account held by run-1)
released
null`;
