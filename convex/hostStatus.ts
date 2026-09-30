/** Independent, opt-in availability reporting for two anonymous home hosts.
 * This module never reads relay devices, account keys or task projections. */
import { httpActionGeneric as httpAction, makeFunctionReference } from "convex/server";
import { v } from "convex/values";

import { internalMutation, internalQuery } from "./server";

type MachineId = "laptop-1" | "laptop-2";
type Health = "ok" | "degraded" | "unknown";
type HostKey = Readonly<{ id: MachineId; label: "laptop 1" | "laptop 2"; tokenSha256: string }>;
type Heartbeat = Readonly<{ version: 1; sequence: number; health: Health; sampleAgeSeconds: number }>;
type StoredHost = Readonly<{
  id: MachineId;
  keyGeneration: string;
  sequence: number;
  lastReceivedAt: number;
  health: Health;
  sampleAgeSeconds: number;
}>;
type RecordArgs = Omit<Heartbeat, "version"> & { id: MachineId; keyGeneration: string };
type RecordOutcome = { status: 204 | 401 | 409 | 429 | 503 };

export const FRESH_MS = 7 * 60_000;
export const OFFLINE_MS = 15 * 60_000;
const MIN_WRITE_MS = 60_000;
const MAX_BODY_BYTES = 1024;
const MAX_CONFIG_BYTES = 1024;
const ENV = "XCB_HOST_STATUS_KEYS";

function plain(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function closed(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const fields = Object.keys(value);
  return fields.length === keys.length && fields.every((field) => keys.includes(field));
}

/** A malformed entry disables the whole configuration. Fixed public aliases
 * prevent a configuration mistake from exposing a hostname or private label. */
export function parseKeys(raw: string | undefined): readonly HostKey[] {
  if (raw === undefined || raw === "") return [];
  if (new TextEncoder().encode(raw).length > MAX_CONFIG_BYTES) throw new Error("invalid host status configuration");
  const values: unknown = JSON.parse(raw);
  if (!Array.isArray(values) || values.length > 2) throw new Error("invalid host status configuration");
  const keys: HostKey[] = [];
  for (const value of values) {
    if (!plain(value) || !closed(value, ["id", "label", "tokenSha256"])
      || (value.id !== "laptop-1" && value.id !== "laptop-2")
      || value.label !== (value.id === "laptop-1" ? "laptop 1" : "laptop 2")
      || typeof value.tokenSha256 !== "string" || !/^[0-9a-f]{64}$/u.test(value.tokenSha256)
      || keys.some((key) => key.id === value.id || key.tokenSha256 === value.tokenSha256)) {
      throw new Error("invalid host status configuration");
    }
    keys.push(value as HostKey);
  }
  return keys.sort((a, b) => a.id.localeCompare(b.id));
}

function configured(): readonly HostKey[] | null {
  try {
    return parseKeys(process.env[ENV]);
  } catch {
    return null;
  }
}

export function parseHeartbeat(value: unknown): Heartbeat | null {
  if (!plain(value) || !closed(value, ["version", "sequence", "health", "sampleAgeSeconds"])
    || value.version !== 1 || !Number.isSafeInteger(value.sequence) || Number(value.sequence) <= 0
    || (value.health !== "ok" && value.health !== "degraded" && value.health !== "unknown")
    || !Number.isInteger(value.sampleAgeSeconds) || Number(value.sampleAgeSeconds) < 0
    || Number(value.sampleAgeSeconds) > 180) return null;
  return value as Heartbeat;
}

/** Compare every character in a fixed-size digest. Do not early-return on the
 * first matching machine: both configured credentials take the same path. */
export function digestEqual(a: string, b: string): boolean {
  if (a.length !== 64 || b.length !== 64) return false;
  let difference = 0;
  for (let index = 0; index < 64; index += 1) difference |= a.charCodeAt(index) ^ b.charCodeAt(index);
  return difference === 0;
}

export async function sha256(value: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function authenticate(request: Request, keys: readonly HostKey[]): Promise<HostKey | null> {
  const bearer = request.headers.get("authorization") ?? "";
  const match = /^Bearer ([0-9a-f]{64})$/u.exec(bearer);
  if (match === null) return null;
  const hash = await sha256(match[1]);
  let matched: HostKey | null = null;
  for (const key of keys) {
    if (digestEqual(hash, key.tokenSha256)) matched = key;
  }
  return matched;
}

const machineValidator = v.union(v.literal("laptop-1"), v.literal("laptop-2"));
const healthValidator = v.union(v.literal("ok"), v.literal("degraded"), v.literal("unknown"));

/** All writes pass through one atomic mutation. Server receipt time, strictly
 * increasing sequence and the one-minute write interval cannot be overridden
 * by a sender, a replay or concurrent requests. Rotation changes the sequence
 * generation in the existing row; it never creates a historical row. */
export const record = internalMutation({
  args: {
    id: machineValidator,
    keyGeneration: v.string(),
    sequence: v.number(),
    health: healthValidator,
    sampleAgeSeconds: v.number(),
  },
  handler: async (ctx, args): Promise<RecordOutcome> => {
    const keys = configured();
    if (keys === null || keys.length === 0) return { status: 503 };
    const key = keys.find((entry) => entry.id === args.id);
    if (key === undefined || !digestEqual(key.tokenSha256, args.keyGeneration)) return { status: 401 };
    if (parseHeartbeat({ version: 1, sequence: args.sequence, health: args.health, sampleAgeSeconds: args.sampleAgeSeconds }) === null) {
      return { status: 409 };
    }
    const rows = (await Promise.all((["laptop-1", "laptop-2"] as const).map(async (id) =>
      await ctx.db.query("xcbHostStatus").withIndex("by_alias", (q) => q.eq("id", id)).take(2)))).flat();
    if (rows.length > 2 || new Set(rows.map((row) => row.id)).size !== rows.length) return { status: 503 };
    const existing = rows.find((row) => row.id === key.id);
    const now = Date.now();
    if (existing !== undefined) {
      if (digestEqual(existing.keyGeneration, key.tokenSha256) && args.sequence <= existing.sequence) return { status: 409 };
      if (now - existing.lastReceivedAt < MIN_WRITE_MS) return { status: 429 };
    }
    const next: StoredHost = {
      id: key.id,
      keyGeneration: key.tokenSha256,
      sequence: args.sequence,
      health: args.health,
      sampleAgeSeconds: args.sampleAgeSeconds,
      lastReceivedAt: now,
    };
    if (existing === undefined) {
      if (rows.length >= 2) return { status: 503 };
      await ctx.db.insert("xcbHostStatus", next);
    } else {
      await ctx.db.replace(existing._id, next);
    }
    return { status: 204 };
  },
});

/** Internal reads return at most the fixed two rows. The HTTP action derives
 * freshness using its current server clock after this query, so a cached
 * database query cannot leave an offline host permanently marked online. */
export const latest = internalQuery({
  args: {},
  handler: async (ctx): Promise<StoredHost[] | null> => {
    const rows = (await Promise.all((["laptop-1", "laptop-2"] as const).map(async (id) =>
      await ctx.db.query("xcbHostStatus").withIndex("by_alias", (q) => q.eq("id", id)).take(2)))).flat();
    if (rows.length > 2 || new Set(rows.map((row) => row.id)).size !== rows.length) return null;
    return rows.map(({ id, keyGeneration, sequence, lastReceivedAt, health, sampleAgeSeconds }) => ({
      id, keyGeneration, sequence, lastReceivedAt, health, sampleAgeSeconds,
    }));
  },
});

export function project(keys: readonly HostKey[], rows: readonly StoredHost[], checkedAt: number) {
  return {
    version: 1 as const,
    configured: keys.length > 0,
    checkedAt,
    machines: keys.map((key) => {
      const row = rows.find((entry) => entry.id === key.id && digestEqual(entry.keyGeneration, key.tokenSha256));
      // A future receipt is unknown after a server clock correction, never a
      // reason to keep a host online longer than the observation supports.
      const valid = row !== undefined && row.lastReceivedAt <= checkedAt;
      const age = valid ? checkedAt - row.lastReceivedAt : Number.POSITIVE_INFINITY;
      const state = !valid ? "never" : age <= FRESH_MS ? "online" : age <= OFFLINE_MS ? "late" : "offline";
      return {
        id: key.id,
        label: key.label,
        lastReceivedAt: valid ? row.lastReceivedAt : null,
        health: valid ? row.health : "unknown" as const,
        state,
        sampleAgeSeconds: valid ? row.sampleAgeSeconds : null,
      };
    }),
  };
}

const recordRef = makeFunctionReference<"mutation", RecordArgs, RecordOutcome>("hostStatus:record");
const latestRef = makeFunctionReference<"query", Record<string, never>, StoredHost[] | null>("hostStatus:latest");

function response(status: number, body?: unknown, cache = false): Response {
  const headers = {
    "Cache-Control": cache ? "public, max-age=60, s-maxage=60" : "no-store",
    "X-Robots-Tag": "noindex",
    "Content-Type": "application/json; charset=utf-8",
    "X-Content-Type-Options": "nosniff",
    ...(status === 429 ? { "Retry-After": "60" } : {}),
  };
  return new Response(body === undefined ? null : JSON.stringify(body), { status, headers });
}

async function readBody(request: Request): Promise<{ value?: unknown; status?: 400 | 413 | 415 }> {
  if ((request.headers.get("content-type") ?? "").split(";", 1)[0]?.trim().toLowerCase() !== "application/json"
    || (request.headers.get("content-encoding") ?? "identity") !== "identity") return { status: 415 };
  const declared = request.headers.get("content-length");
  if (declared !== null && (!/^\d+$/u.test(declared) || Number(declared) > MAX_BODY_BYTES)) return { status: 413 };
  if (request.body === null) return { status: 400 };
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > MAX_BODY_BYTES) {
        await reader.cancel();
        return { status: 413 };
      }
      if (value.byteLength > 0) chunks.push(value);
    }
    const bytes = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    return { value: JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)) as unknown };
  } catch {
    return { status: 400 };
  } finally {
    reader.releaseLock();
  }
}

export const heartbeat = httpAction(async (ctx, request) => {
  const keys = configured();
  if (keys === null || keys.length === 0) return response(503, { error: "host status is unavailable" });
  const key = await authenticate(request, keys);
  if (key === null) return response(401, { error: "unauthorized" });
  const body = await readBody(request);
  if (body.status !== undefined) return response(body.status, { error: "invalid heartbeat" });
  const parsed = parseHeartbeat(body.value);
  if (parsed === null) return response(400, { error: "invalid heartbeat" });
  try {
    const result = await ctx.runMutation(recordRef, {
      id: key.id, keyGeneration: key.tokenSha256, sequence: parsed.sequence,
      health: parsed.health, sampleAgeSeconds: parsed.sampleAgeSeconds,
    });
    return response(result.status);
  } catch {
    return response(503, { error: "host status is unavailable" });
  }
});

export const status = httpAction(async (ctx) => {
  const keys = configured();
  if (keys === null) return response(503, { error: "host status is unavailable" });
  if (keys.length === 0) return response(200, project([], [], Date.now()), true);
  try {
    const rows = await ctx.runQuery(latestRef, {});
    if (rows === null) return response(503, { error: "host status is unavailable" });
    return response(200, project(keys, rows, Date.now()), true);
  } catch {
    return response(503, { error: "host status is unavailable" });
  }
});
