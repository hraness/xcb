import { afterEach, beforeEach, describe, expect, setSystemTime, test } from "bun:test";
import { convexTest } from "convex-test";
import { makeFunctionReference } from "convex/server";

import { FRESH_MS, OFFLINE_MS, activityPerMachine, digestEqual, parseHeartbeat, parseKeys, sha256 } from "./hostStatus";
import schema from "./schema";
import { modules as relayModules } from "./test.setup";

const modules = {
  ...relayModules,
  "./hostStatus.ts": async () => await import("./hostStatus"),
  "./http.ts": async () => await import("./http"),
};
const ENV = "XCB_HOST_STATUS_KEYS";
const original = process.env[ENV];
const START = 1_900_000_000_000;
const TOKEN_A = "a".repeat(64);
const TOKEN_B = "b".repeat(64);
const TOKEN_C = "c".repeat(64);
let HASH_A: string;
let HASH_B: string;

function keys(first = HASH_A) {
  return [
    { id: "laptop-1", label: "laptop 1", tokenSha256: first },
    { id: "laptop-2", label: "laptop 2", tokenSha256: HASH_B },
  ];
}

beforeEach(async () => {
  setSystemTime(new Date(START));
  HASH_A = await sha256(TOKEN_A);
  HASH_B = await sha256(TOKEN_B);
  process.env[ENV] = JSON.stringify(keys());
});

afterEach(() => {
  setSystemTime();
  if (original === undefined) delete process.env[ENV];
  else process.env[ENV] = original;
});

const beat = (sequence = 1) => ({ version: 1, sequence, health: "ok", sampleAgeSeconds: 15 });
type TestWorld = ReturnType<typeof convexTest>;
function post(t: TestWorld, body: unknown = beat(), token = TOKEN_A) {
  return t.fetch("/host-status/heartbeat", {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

describe("anonymous host availability", () => {
  test("configuration is closed, bounded to 100 slug-id machines with free-form labels, and all-or-nothing", () => {
    expect(parseKeys(undefined)).toEqual([]);
    expect(parseKeys("[]")).toEqual([]);
    expect(parseKeys(JSON.stringify(keys()))).toHaveLength(2);
    expect(parseKeys(JSON.stringify([
      { id: "jungle", label: "Ben's MacBook Pro 🖥", tokenSha256: HASH_A },
      { id: "office-mini-2", label: "office mini", tokenSha256: HASH_B },
      { id: "laptop-1", label: "laptop 1 (jungle)", tokenSha256: "c".repeat(64) },
    ]))).toHaveLength(3);
    expect(parseKeys(JSON.stringify(Array.from({ length: 100 }, (_, i) => ({
      id: `machine-${i}`, label: `machine ${i}`, tokenSha256: String(i).padStart(64, "0"),
    }))))).toHaveLength(100);
    for (const invalid of [
      null, {}, [keys()[0], keys()[0]],
      Array.from({ length: 101 }, (_, i) => ({ id: `machine-${i}`, label: "m", tokenSha256: String(i).padStart(64, "0") })),
      [{ ...keys()[0], hostname: "private-name" }],
      [{ ...keys()[0], id: "" }],
      [{ ...keys()[0], id: "-lead" }],
      [{ ...keys()[0], id: "Laptop-1" }],
      [{ ...keys()[0], id: "host.local" }],
      [{ ...keys()[0], id: "a b" }],
      [{ ...keys()[0], id: "a_b" }],
      [{ ...keys()[0], id: `a${"b".repeat(32)}` }],
      [{ ...keys()[0], label: "" }],
      [{ ...keys()[0], label: "   " }],
      [{ ...keys()[0], label: "x".repeat(49) }],
      [{ ...keys()[0], label: "bad\ttab" }],
      [{ ...keys()[0], label: 42 }],
      [{ ...keys()[0], tokenSha256: "not-a-hash" }],
      [keys()[0], { ...keys()[1], tokenSha256: HASH_A }],
    ]) expect(() => parseKeys(JSON.stringify(invalid))).toThrow();
    expect(() => parseKeys(" ".repeat(32 * 1024 + 1))).toThrow();
    expect(digestEqual(HASH_A, HASH_A)).toBe(true);
    expect(digestEqual(HASH_A, HASH_B)).toBe(false);
    expect(digestEqual(HASH_A, "a")).toBe(false);
  });

  test("the shared activity budget keeps the payload bounded as the fleet grows", () => {
    expect(activityPerMachine(1)).toBe(288);
    expect(activityPerMachine(2)).toBe(288);
    expect(activityPerMachine(21)).toBe(288);
    expect(activityPerMachine(22)).toBe(279);
    expect(activityPerMachine(100)).toBe(61);
    expect(activityPerMachine(128)).toBe(48);
    expect(activityPerMachine(1000)).toBe(48);
  });

  test("a third configured machine beats with its own credential and renders in id order", async () => {
    const t = convexTest(schema, modules);
    const third = await sha256(TOKEN_C);
    process.env[ENV] = JSON.stringify([
      { id: "zebra", label: "Zebra", tokenSha256: HASH_B },
      { id: "jungle", label: "Ben's MacBook", tokenSha256: HASH_A },
      { id: "alpha", label: "alpha host", tokenSha256: third },
    ]);
    expect((await post(t, beat(), TOKEN_A)).status).toBe(204);
    expect((await post(t, beat(7), TOKEN_C)).status).toBe(204);
    const payload = await (await t.fetch("/host-status")).json();
    expect(payload.machines.map((m: { id: string }) => m.id)).toEqual(["alpha", "jungle", "zebra"]);
    expect(payload.machines.map((m: { label: string }) => m.label)).toEqual(["alpha host", "Ben's MacBook", "Zebra"]);
    expect(payload.machines[1].state).toBe("online");
    expect(payload.machines[0].state).toBe("online");
    expect(payload.machines[2].state).toBe("never");
    const text = JSON.stringify(payload);
    for (const secret of [TOKEN_A, TOKEN_C, HASH_A, HASH_B, third]) expect(text).not.toContain(secret);
  });

  test("a received heartbeat exposes only the fixed public fields", async () => {
    const t = convexTest(schema, modules);
    expect((await post(t)).status).toBe(204);
    const response = await t.fetch("/host-status");
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toContain("max-age=60");
    expect(response.headers.get("x-robots-tag")).toBe("noindex");
    const payload = await response.json();
    expect(payload).toEqual({
      version: 1, configured: true, checkedAt: START,
      machines: [
        { id: "laptop-1", label: "laptop 1", lastReceivedAt: START, health: "ok", state: "online", sampleAgeSeconds: 15, tasks: { running: 0, queued: 0, needsInput: 0, uncertain: 0 }, resources: { pressure: "unknown", swapUsedBytes: 0, physicalTotalBytes: 0, disksFreeBytes: [] }, activity: [{ observedAt: START, running: 0, queued: 0, needsInput: 0, uncertain: 0 }] },
        { id: "laptop-2", label: "laptop 2", lastReceivedAt: null, health: "unknown", state: "never", sampleAgeSeconds: null, tasks: { running: 0, queued: 0, needsInput: 0, uncertain: 0 }, resources: { pressure: "unknown", swapUsedBytes: 0, physicalTotalBytes: 0, disksFreeBytes: [] }, activity: [] },
      ],
    });
    const text = JSON.stringify(payload);
    for (const secret of [TOKEN_A, HASH_A, HASH_B, "keyGeneration", "sequence"]) expect(text).not.toContain(secret);
    const rows = await t.run(async (ctx) => await ctx.db.query("xcbHostStatus").collect());
    expect(rows).toHaveLength(1);
    expect(rows[0]?.sequence).toBe(1);
    // The existing relay tables were neither removed nor populated.
    expect(await t.run(async (ctx) => await ctx.db.query("relayDevices").collect())).toEqual([]);
    expect(Object.keys(schema.tables)).toContain("relaySubjects");
  });

  test("unauthorized, oversized and open-ended requests never create rows", async () => {
    const t = convexTest(schema, modules);
    expect((await post(t, beat(), TOKEN_C)).status).toBe(401);
    expect((await t.fetch("/host-status/heartbeat", { method: "POST", body: "{}" })).status).toBe(401);
    for (const value of [
      { ...beat(), sequence: 0 }, { ...beat(), sequence: Number.MAX_SAFE_INTEGER + 1 },
      { ...beat(), sequence: 1.5 }, { ...beat(), sampleAgeSeconds: 181 },
      { ...beat(), sampleAgeSeconds: -1 }, { ...beat(), sampleAgeSeconds: 0.5 },
      { ...beat(), id: "laptop-2" }, { ...beat(), observedAt: START },
      { ...beat(), health: "healthy" }, { ...beat(), version: 2 }, null,
    ]) {
      expect(parseHeartbeat(value)).toBeNull();
      expect((await post(t, value)).status).toBe(400);
    }
    const large = await t.fetch("/host-status/heartbeat", {
      method: "POST",
      headers: { authorization: `Bearer ${TOKEN_A}`, "content-type": "application/json", "content-length": "0" },
      body: " ".repeat(1025),
    });
    expect(large.status).toBe(413);
    expect(await t.run(async (ctx) => await ctx.db.query("xcbHostStatus").collect())).toEqual([]);
  });

  test("duplicates and stale sequences cannot refresh the server receipt", async () => {
    const t = convexTest(schema, modules);
    expect((await post(t, beat(10))).status).toBe(204);
    setSystemTime(new Date(START + 300_000));
    expect((await post(t, beat(10))).status).toBe(409);
    expect((await post(t, beat(9))).status).toBe(409);
    let data = await (await t.fetch("/host-status")).json();
    expect(data.machines[0].lastReceivedAt).toBe(START);
    expect((await post(t, beat(11))).status).toBe(204);
    data = await (await t.fetch("/host-status")).json();
    expect(data.machines[0].lastReceivedAt).toBe(START + 300_000);
  });

  test("the minimum write interval is per machine and does not consume a rejected sequence", async () => {
    const t = convexTest(schema, modules);
    expect((await post(t)).status).toBe(204);
    expect((await post(t, beat(), TOKEN_B)).status).toBe(204);
    setSystemTime(new Date(START + 59_999));
    const limited = await post(t, beat(2));
    expect(limited.status).toBe(429);
    expect(limited.headers.get("retry-after")).toBe("60");
    setSystemTime(new Date(START + 60_000));
    expect((await post(t, beat(2))).status).toBe(204);
    expect(await t.run(async (ctx) => (await ctx.db.query("xcbHostStatus").collect()).length)).toBe(2);
  });

  test("concurrent deliveries of one sequence commit once", async () => {
    const t = convexTest(schema, modules);
    const replies = await Promise.all([post(t), post(t)]);
    expect(replies.map((reply) => reply.status).sort()).toEqual([204, 409]);
    expect(await t.run(async (ctx) => (await ctx.db.query("xcbHostStatus").collect()).length)).toBe(1);
  });

  test("credential rotation hides the old receipt and resets sequence in the same row", async () => {
    const t = convexTest(schema, modules);
    expect((await post(t, beat(100))).status).toBe(204);
    const nextHash = await sha256(TOKEN_C);
    process.env[ENV] = JSON.stringify(keys(nextHash));
    const rotated = await (await t.fetch("/host-status")).json();
    expect(rotated.machines[0].state).toBe("never");
    expect(rotated.machines[0].lastReceivedAt).toBeNull();
    expect((await post(t, beat(101))).status).toBe(401);
    expect((await post(t, beat(1), TOKEN_C)).status).toBe(429);
    setSystemTime(new Date(START + 60_000));
    // A request authenticated before rotation cannot commit afterward.
    const record = makeFunctionReference<"mutation">("hostStatus:record");
    expect(await t.mutation(record, { id: "laptop-1", keyGeneration: HASH_A, sequence: 101, health: "ok", sampleAgeSeconds: 1 })).toEqual({ status: 401 });
    expect((await post(t, beat(1), TOKEN_C)).status).toBe(204);
    const rows = await t.run(async (ctx) => await ctx.db.query("xcbHostStatus").collect());
    expect(rows).toHaveLength(1);
    expect(rows[0]?.sequence).toBe(1);
    expect(rows[0]?.keyGeneration).toBe(nextHash);
  });

  test("time passing alone changes online to late and offline", async () => {
    const t = convexTest(schema, modules);
    expect((await post(t)).status).toBe(204);
    for (const [age, state] of [[FRESH_MS, "online"], [FRESH_MS + 1, "late"], [OFFLINE_MS, "late"], [OFFLINE_MS + 1, "offline"]] as const) {
      setSystemTime(new Date(START + age));
      const payload = await (await t.fetch("/host-status")).json();
      expect(payload.checkedAt).toBe(START + age);
      expect(payload.machines[0].state).toBe(state);
      expect(payload.machines[0].lastReceivedAt).toBe(START);
    }
  });

  test("absent configuration is truthful and malformed configuration is unavailable", async () => {
    const t = convexTest(schema, modules);
    delete process.env[ENV];
    expect(await (await t.fetch("/host-status")).json()).toEqual({ version: 1, configured: false, checkedAt: START, machines: [] });
    expect((await post(t)).status).toBe(503);
    process.env[ENV] = JSON.stringify([keys()[0], { ...keys()[1], tokenSha256: "bad" }]);
    const unavailable = await t.fetch("/host-status");
    expect(unavailable.status).toBe(503);
    expect(await unavailable.text()).not.toContain(HASH_A);
    expect((await post(t)).status).toBe(503);
  });

  test("unexpected duplicate rows fail closed and are preserved for inspection", async () => {
    const t = convexTest(schema, modules);
    expect((await post(t)).status).toBe(204);
    await t.run(async (ctx) => {
      await ctx.db.insert("xcbHostStatus", { id: "laptop-1", keyGeneration: HASH_A, sequence: 2, lastReceivedAt: START, health: "ok", sampleAgeSeconds: 0 });
    });
    expect((await t.fetch("/host-status")).status).toBe(503);
    setSystemTime(new Date(START + 60_000));
    expect((await post(t, beat(3))).status).toBe(503);
    expect(await t.run(async (ctx) => (await ctx.db.query("xcbHostStatus").collect()).length)).toBe(2);
  });
});
