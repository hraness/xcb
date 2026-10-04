import { readFileSync } from "node:fs";
import { expect, test } from "bun:test";
import {
  DEFAULT_SHADOW_MANIFEST,
  applyShadowCommand,
  counterfactualReceiptCheck,
  createShadowState,
  replayShadowCommands,
  runSeededFaultBattery,
  verifyDeterministicReplay,
  verifyShadowHistory,
} from "../src/assurance.ts";
import type { ShadowCommand } from "../src/assurance.ts";
import type { ShadowReceipt, ShadowReceiptStatus } from "../src/assurance.ts";

type Vector = Readonly<{
  name: string;
  commands: readonly ShadowCommand[];
  expectedStatuses?: readonly ShadowReceiptStatus[];
  expectedRevisions?: readonly (number | null)[];
  expectedErrors?: readonly ShadowReceipt["errorCode"][];
  error?: string;
}>;
type VectorFile = Readonly<{
  schema: string;
  version: number;
  golden: readonly Vector[];
  negative: readonly Vector[];
}>;
const vectors = JSON.parse(readFileSync(new URL("../protocol/assurance-v1-vectors.json", import.meta.url), "utf8")) as VectorFile;

for (const vector of vectors.golden) {
  test(`assurance golden vector: ${vector.name}`, () => {
    const replay = replayShadowCommands(vector.commands);
    if (vector.expectedStatuses !== undefined) expect([...replay.receipts.map(receipt => receipt.status)]).toEqual([...vector.expectedStatuses]);
    if (vector.expectedRevisions !== undefined) expect([...replay.receipts.map(receipt => receipt.revision)]).toEqual([...vector.expectedRevisions]);
    if (vector.expectedErrors !== undefined) expect([...replay.receipts.map(receipt => receipt.errorCode)]).toEqual([...vector.expectedErrors]);
    expect(verifyShadowHistory(replay.state)).toBe(true);
  });
}

for (const vector of vectors.negative) {
  test(`assurance negative vector: ${vector.name}`, () => {
    expect(() => replayShadowCommands(vector.commands)).toThrow(vector.error);
  });
}

test("shadow receipts are deterministic and counterfactual manifests are explicit", () => {
  const commands: ShadowCommand[] = [{
    commandId: "cmd_check_01",
    target: "task.fixture",
    command: "task/accept",
    arguments: { value: 1 },
    expectedRevision: 0,
    idempotencyKey: "idem_check_01",
  }];
  const replay = verifyDeterministicReplay(commands);
  expect(replay.identical).toBe(true);
  expect(replay.differingReceiptIndex).toBeNull();
  const changed = counterfactualReceiptCheck(commands, { routing: "route.fixture.v2", projection: DEFAULT_SHADOW_MANIFEST.projection });
  expect(changed.identical).toBe(false);
  expect(changed.difference).toBe("manifest");
  expect(changed.differingReceiptIndex).toBe(0);
});

test("idempotency and stale revisions never apply a second effect", () => {
  const command: ShadowCommand = {
    commandId: "cmd_invariant_01",
    target: "task.fixture",
    command: "task/accept",
    arguments: { value: true },
    expectedRevision: 0,
    idempotencyKey: "idem_invariant_01",
  };
  const first = applyShadowCommand(createShadowState(), command);
  const replay = applyShadowCommand(first.state, command);
  expect(replay.receipt.status).toBe("replayed");
  expect(replay.state.targets[0]?.revision).toBe(1);
  const changed = applyShadowCommand(replay.state, { ...command, commandId: "cmd_invariant_02", arguments: { value: false } });
  expect(changed.receipt.errorCode).toBe("idempotency_conflict");
  expect(changed.state.targets[0]?.revision).toBe(1);
});

test("the named crash/restart/storage-fault battery is reproducible", () => {
  const first = runSeededFaultBattery();
  const second = runSeededFaultBattery();
  expect(first).toEqual(second);
  expect(first).toHaveLength(6);
  expect(first.every(result => result.passed)).toBe(true);
  expect(first.find(result => result.name === "restart-before-settle")?.receipts[1]?.status).toBe("replayed");
});
