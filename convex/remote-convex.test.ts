import { describe, expect, test } from "bun:test";
import { convexTest } from "convex-test";
import { makeFunctionReference } from "convex/server";

import { createDeviceIdentity, signCanonicalBase64 } from "@hraness/relay/crypto";
import { uuidV7 } from "@hraness/relay/wire";

import schema from "./schema";
import { modules } from "./test.setup";

const register = makeFunctionReference<"mutation">("relayDevices:register");
const beginBind = makeFunctionReference<"mutation">("relayDevices:beginBind");
const finishBind = makeFunctionReference<"mutation">("relayDevices:finishBind");
const enqueue = makeFunctionReference<"mutation">("relayCommands:enqueue");
const get = makeFunctionReference<"query">("relayCommands:get");

const sampleEnvelope = (sender: string, recipient: string) => ({
  ciphertext: "A".repeat(48),
  contract: "xcb.relay.v1",
  iv: "B".repeat(16),
  keyVersion: 1,
  recipient,
  scope: "commands.v1",
  sender,
  signature: "A".repeat(86),
});

async function enrolledWorld() {
  const t = convexTest(schema, modules);
  const now = Date.now();
  const { userId, authSessionId } = await t.run(async (ctx) => {
    const userId = await ctx.db.insert("users", {
      email: "owner@example.test",
      emailVerificationTime: now,
    });
    await ctx.db.insert("relaySubjects", {
      authEpoch: 1,
      createdAt: now,
      emailDigest: "0".repeat(64),
      status: "active",
      updatedAt: now,
      userId,
      verifiedAt: now,
    });
    const authSessionId = await ctx.db.insert("authSessions", {
      expirationTime: now + 3_600_000,
      userId,
    });
    return { authSessionId, userId };
  });
  const runtime = t.withIdentity({
    issuer: "https://test.example",
    subject: `${userId}|${authSessionId}`,
    tokenIdentifier: `test|${authSessionId}`,
  });
  const device = await createDeviceIdentity();
  await runtime.mutation(register, {
    agreementPublicKey: device.publicKeys.agreement,
    deviceClass: "daemon",
    deviceId: device.device,
    label: "laptop",
    signingPublicKey: device.publicKeys.signing,
  });
  const begin = await runtime.mutation(beginBind, { deviceId: device.device }) as { challengeId: string; nonce: string };
  const signature = await signCanonicalBase64(device.signing.privateKey, {
    challengeId: begin.challengeId,
    contract: "xcb.relay.v1:device-bind",
    nonce: begin.nonce,
  });
  await runtime.mutation(finishBind, {
    challengeId: begin.challengeId,
    deviceId: device.device,
    signature,
  });
  return { authSessionId, device, deviceId: device.device, runtime, t, userId };
}

describe("xcb relay instantiation", () => {
  test("enrolls a daemon through the real bind ceremony under xcb.relay.v1", async () => {
    const { deviceId } = await enrolledWorld();
    expect(deviceId).toMatch(/^[0-9a-f]{32}$/);
  });

  test("rejects a device class outside the union", async () => {
    const t = convexTest(schema, modules);
    const now = Date.now();
    const { userId, authSessionId } = await t.run(async (ctx) => {
      const userId = await ctx.db.insert("users", { email: "owner@example.test", emailVerificationTime: now });
      const authSessionId = await ctx.db.insert("authSessions", { expirationTime: now + 3_600_000, userId });
      return { authSessionId, userId };
    });
    const runtime = t.withIdentity({
      issuer: "https://test.example",
      subject: `${userId}|${authSessionId}`,
      tokenIdentifier: `test|${authSessionId}`,
    });
    const device = await createDeviceIdentity();
    await expect(runtime.mutation(register, {
      agreementPublicKey: device.publicKeys.agreement,
      deviceClass: "browser",
      deviceId: device.device,
      label: "web",
      signingPublicKey: device.publicKeys.signing,
    })).rejects.toThrow();
  });

  test("admits the frozen command union and rejects unknown kinds", async () => {
    const { deviceId, t, userId } = await enrolledWorld();
    const controller = await createDeviceIdentity();
    // A device binds to exactly one auth session — the controller gets its own.
    const controllerSession = await t.run(async (ctx) =>
      await ctx.db.insert("authSessions", { expirationTime: Date.now() + 3_600_000, userId }));
    const controllerRuntime = t.withIdentity({
      issuer: "https://test.example",
      subject: `${userId}|${controllerSession}`,
      tokenIdentifier: `test|${controllerSession}`,
    });
    await controllerRuntime.mutation(register, {
      agreementPublicKey: controller.publicKeys.agreement,
      deviceClass: "controller",
      deviceId: controller.device,
      label: "cli",
      signingPublicKey: controller.publicKeys.signing,
    });
    const begin = await controllerRuntime.mutation(beginBind, { deviceId: controller.device }) as { challengeId: string; nonce: string };
    const signature = await signCanonicalBase64(controller.signing.privateKey, {
      challengeId: begin.challengeId,
      contract: "xcb.relay.v1:device-bind",
      nonce: begin.nonce,
    });
    await controllerRuntime.mutation(finishBind, {
      challengeId: begin.challengeId,
      deviceId: controller.device,
      signature,
    });

    const payload = sampleEnvelope(controller.device, deviceId);
    const enqueued = await controllerRuntime.mutation(enqueue, {
      idempotencyKey: uuidV7(),
      kind: "task_dispatch",
      payload,
      requestDigest: `sha256:${"a".repeat(64)}`,
      targetDeviceId: deviceId,
    }) as { command: { publicId: string } };
    const stored = await controllerRuntime.query(get, { publicId: enqueued.command.publicId }) as { kind: string };
    expect(stored.kind).toBe("task_dispatch");

    await expect(controllerRuntime.mutation(enqueue, {
      idempotencyKey: uuidV7(),
      kind: "rm_everything",
      payload,
      requestDigest: `sha256:${"b".repeat(64)}`,
      targetDeviceId: deviceId,
    })).rejects.toThrow();
  });
});
