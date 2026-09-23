import type { AccountLeaseStore } from "./accounts.ts";
import type { CapabilityBroker } from "./capabilities.ts";
import {
  runAgentTask,
  type AgentTaskAdapter,
  type AgentTaskRequest,
  type AgentTaskResult,
  type AgentTaskRoute,
} from "./task-runtime.ts";
import { identifier, provider, type AgentProvider } from "./validation.ts";

const NEVER = new AbortController().signal;

/**
 * The caller-facing request: `route` resolves by `provider` + `authentication`
 * against the registered adapters when it is not given exactly, and `runId` /
 * `workspaceId` default to the broker's own binding. Every other field keeps
 * the task-runtime contract — the router validates nothing the runtime would
 * not recheck.
 */
export type RouterTaskRequest = Omit<AgentTaskRequest, "route" | "runId" | "workspaceId" | "signal"> &
  Readonly<{
    /** Exact route object; required when one provider registers several. */
    route?: AgentTaskRoute;
    /** Resolve the registered route for this provider. */
    provider?: AgentProvider;
    /** With `provider`, selects the route's authentication kind. */
    authentication?: AgentTaskRoute["authentication"];
    runId?: string;
    workspaceId?: string;
    signal?: AbortSignal;
  }>;

export type SubscriptionRouter = Readonly<{
  /**
   * The adapter-registered routes this router can dispatch to. Registration is
   * not eligibility: account custody, credentials and qualification evidence
   * are proven when a task runs, never by listing.
   */
  routes(): readonly AgentTaskRoute[];
  run(request: RouterTaskRequest, broker: CapabilityBroker): Promise<AgentTaskResult>;
}>;

export type SubscriptionRouterOptions = Readonly<{
  /** Host-owned custody store, e.g. `new SqliteAccountLeases(db)`. */
  leases: AccountLeaseStore;
  adapters: readonly AgentTaskAdapter[];
  now?: () => number;
}>;

function resolveRoute(
  adapters: readonly AgentTaskAdapter[],
  input: RouterTaskRequest,
): AgentTaskRoute {
  if (input.route !== undefined) {
    const selected = input.route;
    if (input.provider !== undefined && provider(input.provider) !== provider(selected.provider))
      throw new Error("ROUTER_ROUTE_PROVIDER_MISMATCH");
    return Object.freeze({
      id: identifier(selected.id),
      provider: provider(selected.provider),
      authentication: selected.authentication,
    });
  }
  if (input.provider === undefined) throw new Error("ROUTER_ROUTE_REQUIRED");
  const wanted = { provider: provider(input.provider), authentication: input.authentication ?? "subscription" };
  if (wanted.authentication !== "subscription" && wanted.authentication !== "api")
    throw new Error("ROUTER_AUTHENTICATION_INVALID");
  const matches = adapters.filter(
    (adapter) => adapter.route.provider === wanted.provider && adapter.route.authentication === wanted.authentication,
  );
  if (matches.length === 0) throw new Error("ROUTER_ROUTE_UNAVAILABLE");
  if (matches.length > 1) throw new Error("ROUTER_ROUTE_AMBIGUOUS");
  return matches[0]!.route;
}

/**
 * One construction call for the embeddable subscription router: an account
 * lease store plus qualified task adapters. There is no bundled live adapter
 * and no credential discovery — the host still supplies both.
 */
export function createSubscriptionRouter(options: SubscriptionRouterOptions): SubscriptionRouter {
  if (options.leases === null || typeof options.leases !== "object") throw new Error("ROUTER_LEASES_REQUIRED");
  for (const method of ["acquire", "renew", "release"] as const)
    if (typeof options.leases[method] !== "function") throw new Error("ROUTER_LEASES_INVALID");
  if (!Array.isArray(options.adapters)) throw new Error("ROUTER_ADAPTERS_INVALID");
  const adapters = Object.freeze([...options.adapters]);
  const ids = new Set<string>();
  for (const adapter of adapters) {
    const route = adapter?.route;
    if (route === null || typeof route !== "object") throw new Error("ROUTER_ADAPTER_INVALID");
    const id = identifier(route.id);
    provider(route.provider);
    if (route.authentication !== "subscription" && route.authentication !== "api")
      throw new Error("ROUTER_AUTHENTICATION_INVALID");
    if (ids.has(id)) throw new Error("ROUTER_DUPLICATE_ROUTE");
    ids.add(id);
  }
  const leases = options.leases;
  const now = options.now ?? (() => Date.now());
  return Object.freeze({
    routes: () => adapters.map((adapter) => adapter.route),
    async run(input: RouterTaskRequest, broker: CapabilityBroker): Promise<AgentTaskResult> {
      if (broker === null || typeof broker !== "object" || typeof broker.invoke !== "function")
        throw new Error("ROUTER_BROKER_INVALID");
      const base = {
        route: resolveRoute(adapters, input),
        accountId: input.accountId,
        workspaceId: input.workspaceId ?? broker.workspaceId,
        runId: input.runId ?? broker.runId,
        profile: input.profile,
        model: input.model,
        purpose: input.purpose,
        prompt: input.prompt,
        limits: input.limits,
        signal: input.signal ?? NEVER,
      };
      const request: AgentTaskRequest = Object.freeze(
        input.authority === undefined ? base : { ...base, authority: input.authority },
      );
      return runAgentTask({ adapters, leases, now }, request, broker);
    },
  });
}
