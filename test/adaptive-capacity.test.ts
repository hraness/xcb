import { describe, expect, test } from "bun:test";
import { nextAdaptiveCapacity, type CapacitySignals } from "../src/adaptive-capacity.ts";

const healthy: CapacitySignals = { availableRuns: 16, availableMemoryBytes: 10_000, availableDiskBytes: 10_000,
  quota: "healthy", memory: "healthy", disk: "healthy", providerFailures: 0, uncertainRuns: 0 };
const config = { floor: 1, ceiling: 8, step: 1, minimumMemoryBytes: 100, minimumDiskBytes: 100 } as const;

describe("adaptive capacity", () => {
  test("ramps conservatively toward the ceiling", () => {
    expect(nextAdaptiveCapacity(1, config, healthy).target).toBe(2);
    expect(nextAdaptiveCapacity(2, config, healthy).target).toBe(4);
    expect(nextAdaptiveCapacity(4, config, healthy).target).toBe(8);
    expect(nextAdaptiveCapacity(8, config, healthy).direction).toBe("hold");
  });

  test("backs off on pressure and exhausted quota", () => {
    expect(nextAdaptiveCapacity(8, config, { ...healthy, memory: "pressured" })).toMatchObject({ target: 4, direction: "decrease", reason: "memory-pressure" });
    expect(nextAdaptiveCapacity(2, config, { ...healthy, quota: "exhausted" })).toMatchObject({ target: 1, direction: "decrease", reason: "quota-exhausted" });
  });

  test("holds when telemetry is unknown", () => {
    expect(nextAdaptiveCapacity(4, config, { ...healthy, disk: "unknown" })).toMatchObject({ target: 4, direction: "hold", reason: "telemetry-unknown" });
  });
});
