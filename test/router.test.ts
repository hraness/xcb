import { describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { SqliteAccountLeases } from "../src/accounts.ts";
import { createCapabilityBroker, createCapabilityProfile } from "../src/capabilities.ts";
import { createSubscriptionRouter, type RouterTaskRequest } from "../src/router.ts";
import type { AgentTaskAdapter, AgentTaskBinding, AgentTaskCompletion, AgentTaskExecutionRequest,
  AgentTaskStopEvidence, TaskRuntimeQualification } from "../src/task-runtime.ts";

const hash = (char: string) => char.repeat(64);

function fixture() {
  const db = new Database(":memory:");
  const leases = new SqliteAccountLeases(db);
  const capability = createCapabilityProfile({ id: "synthetic.research", version: 1, tools: [] });
  const profile = { id: capability.id, version: capability.version, digest: capability.digest };
  const broker = createCapabilityBroker({ profile: capability, workspaceId: "workspace-one", runId: "run-one", isActive: () => true });
  const request: RouterTaskRequest = { provider: "codex", accountId: "account-one", profile,
    model: { id: "synthetic-model", reasoningEffort: null, serviceTier: null }, purpose: "research",
    prompt: "Synthetic bounded task", limits: { maxRunMs: 10_000, maxCleanupMs: 1_000, maxOutputBytes: 64 } };
  return { db, leases, capability, profile, broker, request };
}

function adapter(route: AgentTaskAdapter["route"], qualification: TaskRuntimeQualification,
  record?: { calls: number }): AgentTaskAdapter {
  const binding = (r: AgentTaskExecutionRequest): AgentTaskBinding => ({ route: r.route, accountId: r.accountId,
    workspaceId: r.workspaceId, runId: r.runId, profile: r.profile, model: r.model, runtime: r.runtime, accountLease: r.accountLease });
  return { route, runtime: { version: "synthetic-only", digest: hash("a") }, qualification,
    async run(r) { if (record) record.calls++; const base: AgentTaskCompletion = { ...binding(r), output: "synthetic answer",
      outcome: { status: "completed", code: null }, usage: { inputTokens: null, outputTokens: null, totalTokens: null, costUsd: null } };
      return base; },
    async stop(r): Promise<AgentTaskStopEvidence> { return { ...binding(r), processStopped: true, controllersStopped: true,
      joined: true, stoppedAtUnixMs: r.admittedAtUnixMs, proofDigest: hash("c") }; } };
}

function qualified(route: AgentTaskAdapter["route"], profile: { id: string; version: number; digest: string }): TaskRuntimeQualification {
  return { status: "qualified", route, profile, runtimeVersion: "synthetic-only", runtimeDigest: hash("a"),
    evidenceDigest: hash("b"), expiresAt: 100_000,
    controls: { noCommandTools: true, exactToolInventory: true, workspaceReadIsolation: true, workspaceWriteIsolation: true,
      isolatedConfiguration: true, authOutsideWorkspace: true, hostBrokerOnly: true } };
}

describe("subscription router", () => {
  test("rejects duplicate and malformed adapter routes at construction", () => {
    const f = fixture();
    const route = { id: "codex-subscription", provider: "codex", authentication: "subscription" } as const;
    const one = adapter(route, qualified(route, f.profile));
    expect(() => createSubscriptionRouter({ leases: f.leases, adapters: [one, one] })).toThrow("ROUTER_DUPLICATE_ROUTE");
    expect(() => createSubscriptionRouter({ leases: f.leases, adapters: [{ route: { id: "x", provider: "paper", authentication: "subscription" } } as never] })).toThrow("UNSUPPORTED_PROVIDER");
    f.db.close();
  });

  test("lists registered routes and resolves provider shorthand", async () => {
    const f = fixture();
    const route = { id: "codex-subscription", provider: "codex", authentication: "subscription" } as const;
    const calls = { calls: 0 };
    const router = createSubscriptionRouter({ leases: f.leases, adapters: [adapter(route, qualified(route, f.profile), calls)], now: () => 1_000 });
    expect(router.routes()).toEqual([route]);
    const result = await router.run(f.request, f.broker);
    expect(calls.calls).toBe(1);
    expect(result.runId).toBe("run-one");
    expect(result.workspaceId).toBe("workspace-one");
    expect(result.outcome.status).toBe("completed");
    f.db.close();
  });

  test("ambiguous provider routes require an exact route object", async () => {
    const f = fixture();
    const a = { id: "codex-subscription", provider: "codex", authentication: "subscription" } as const;
    const b = { id: "codex-managed", provider: "codex", authentication: "subscription" } as const;
    const router = createSubscriptionRouter({ leases: f.leases,
      adapters: [adapter(a, qualified(a, f.profile)), adapter(b, qualified(b, f.profile))], now: () => 1_000 });
    await expect(router.run(f.request, f.broker)).rejects.toThrow("ROUTER_ROUTE_AMBIGUOUS");
    const explicit = await router.run({ ...f.request, route: b }, f.broker);
    expect(explicit.route.id).toBe("codex-managed");
    f.db.close();
  });

  test("unregistered providers and mismatched route/provider pairs fail closed", async () => {
    const f = fixture();
    const route = { id: "codex-subscription", provider: "codex", authentication: "subscription" } as const;
    const router = createSubscriptionRouter({ leases: f.leases, adapters: [adapter(route, qualified(route, f.profile))] });
    await expect(router.run({ ...f.request, provider: "claude" }, f.broker)).rejects.toThrow("ROUTER_ROUTE_UNAVAILABLE");
    const { provider: _provider, ...withoutProvider } = f.request;
    await expect(router.run(withoutProvider, f.broker)).rejects.toThrow("ROUTER_ROUTE_REQUIRED");
    const claude = { id: "claude-subscription", provider: "claude", authentication: "subscription" } as const;
    await expect(router.run({ ...f.request, provider: "codex", route: claude }, f.broker)).rejects.toThrow("ROUTER_ROUTE_PROVIDER_MISMATCH");
    f.db.close();
  });
});
