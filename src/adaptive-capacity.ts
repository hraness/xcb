import { safeInteger } from "./validation.ts";

/** Signals used to choose a target; they never grant capacity by themselves. */
export type CapacitySignals = Readonly<{
  availableRuns: number;
  availableMemoryBytes: number;
  availableDiskBytes: number;
  quota: "healthy" | "exhausted" | "unknown";
  memory: "healthy" | "pressured" | "critical" | "unknown";
  disk: "healthy" | "pressured" | "critical" | "unknown";
  providerFailures: number;
  uncertainRuns: number;
}>;

export type AdaptiveCapacityConfig = Readonly<{
  floor: number;
  ceiling: number;
  step: number;
  minimumMemoryBytes: number;
  minimumDiskBytes: number;
}>;

export type AdaptiveCapacityDecision = Readonly<{
  target: number;
  previous: number;
  direction: "increase" | "decrease" | "hold";
  reason:
    | "capacity-available"
    | "memory-pressure"
    | "disk-pressure"
    | "quota-exhausted"
    | "provider-failures"
    | "telemetry-unknown"
    | "within-target"
    | "floor-or-ceiling";
}>;

/**
 * Choose a bounded concurrency target. This is intentionally pure and
 * conservative: a target is advisory and every task still needs a durable
 * reservation before launch.
 */
export function nextAdaptiveCapacity(
  current: number,
  config: AdaptiveCapacityConfig,
  signals: CapacitySignals,
): AdaptiveCapacityDecision {
  validateConfig(config);
  const previous = safeInteger(current, config.floor, config.ceiling);
  validateSignals(signals);

  const hardPressure = signals.memory === "critical" || signals.disk === "critical";
  const softPressure = signals.memory === "pressured" || signals.disk === "pressured";
  const unknown = signals.quota === "unknown" || signals.memory === "unknown" || signals.disk === "unknown";
  const lowReserve = signals.availableMemoryBytes < config.minimumMemoryBytes
    || signals.availableDiskBytes < config.minimumDiskBytes;

  if (hardPressure || lowReserve) return decision(previous, Math.max(config.floor, Math.floor(previous / 2)), "decrease",
    signals.memory === "critical" || signals.availableMemoryBytes < config.minimumMemoryBytes ? "memory-pressure" : "disk-pressure");
  if (signals.quota === "exhausted") return decision(previous, Math.max(config.floor, Math.floor(previous / 2)), "decrease", "quota-exhausted");
  if (signals.providerFailures > 0) return decision(previous, Math.max(config.floor, Math.floor(previous / 2)), "decrease", "provider-failures");
  if (softPressure) return decision(previous, Math.max(config.floor, Math.floor(previous / 2)), "decrease",
    signals.memory === "pressured" ? "memory-pressure" : "disk-pressure");
  if (unknown) return decision(previous, previous, "hold", "telemetry-unknown");

  const available = Math.min(config.ceiling, signals.availableRuns + previous);
  if (available <= previous) return decision(previous, previous, "hold", "within-target");
  const next = Math.min(config.ceiling, available, Math.max(previous + config.step, previous * 2));
  return decision(previous, next, next > previous ? "increase" : "hold", next > previous ? "capacity-available" : "floor-or-ceiling");
}

function decision(previous: number, target: number, direction: AdaptiveCapacityDecision["direction"], reason: AdaptiveCapacityDecision["reason"]): AdaptiveCapacityDecision {
  return Object.freeze({ previous, target, direction, reason });
}

function validateConfig(config: AdaptiveCapacityConfig): void {
  safeInteger(config.floor, 1, 10_000); safeInteger(config.ceiling, config.floor, 10_000); safeInteger(config.step, 1, 10_000);
  safeInteger(config.minimumMemoryBytes, 0, Number.MAX_SAFE_INTEGER); safeInteger(config.minimumDiskBytes, 0, Number.MAX_SAFE_INTEGER);
}

function validateSignals(signals: CapacitySignals): void {
  safeInteger(signals.availableRuns, 0, 10_000); safeInteger(signals.availableMemoryBytes, 0, Number.MAX_SAFE_INTEGER);
  safeInteger(signals.availableDiskBytes, 0, Number.MAX_SAFE_INTEGER); safeInteger(signals.providerFailures, 0, 10_000);
  safeInteger(signals.uncertainRuns, 0, 10_000);
  if (!["healthy", "exhausted", "unknown"].includes(signals.quota)
    || !["healthy", "pressured", "critical", "unknown"].includes(signals.memory)
    || !["healthy", "pressured", "critical", "unknown"].includes(signals.disk)) throw new Error("CAPACITY_SIGNAL_INVALID");
}
