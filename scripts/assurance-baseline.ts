import { writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { canonicalJson } from "../src/canonical-json.ts";
import {
  DEFAULT_SHADOW_MANIFEST,
  applyShadowCommand,
  createShadowState,
  replayShadowCommands,
} from "../src/assurance.ts";
import type { ShadowCommand } from "../src/assurance.ts";

const DEFAULT_SEED = 20_737;
const DEFAULT_COMMANDS = 256;
const DEFAULT_TARGETS = 4;
const DEFAULT_WARMUP = 16;
const DEFAULT_REPETITIONS = 31;

type Summary = Readonly<{
  median: number;
  p95: number;
  minimum: number;
  maximum: number;
  samples: number;
}>;

type BaselineReceipt = Readonly<{
  schema: "xcb.metrics.baseline-receipt.v1";
  status: "measured";
  generatedAt: string;
  workload: Readonly<{
    id: "xcb-shadow-acceptance-v1";
    seed: number;
    commands: number;
    targets: number;
    warmup: number;
    repetitions: number;
    manifest: typeof DEFAULT_SHADOW_MANIFEST;
  }>;
  source: Readonly<{ node: string; bun: string | null }>;
  metrics: Readonly<{
    acceptanceLatencyUs: Summary;
    projectionBytes: Summary;
    cpuUs: Summary;
    memoryBytes: Summary;
    journalGrowthBytes: Summary;
    replayTimeUs: Summary;
  }>;
  notes: readonly string[];
}>;

function argument(name: string, fallback: number): number {
  const index = process.argv.indexOf(name);
  if (index < 0) return fallback;
  const value = Number(process.argv[index + 1]);
  if (!Number.isSafeInteger(value) || value < 1) throw new Error(`invalid ${name}`);
  return value;
}

function summary(values: readonly number[]): Summary {
  if (values.length === 0) throw new Error("no baseline samples");
  const sorted = [...values].sort((left, right) => left - right);
  const at = (fraction: number): number => sorted[Math.min(sorted.length - 1, Math.floor((sorted.length - 1) * fraction))]!;
  return { median: at(0.5), p95: at(0.95), minimum: sorted[0]!, maximum: sorted[sorted.length - 1]!, samples: values.length };
}

function workload(seed: number, count: number, targets: number): readonly ShadowCommand[] {
  const commands: ShadowCommand[] = [];
  const revisions = new Array<number>(targets).fill(0);
  for (let index = 0; index < count; index += 1) {
    const targetIndex = index % targets;
    const target = `task.fixture.${targetIndex}`;
    const revision = revisions[targetIndex]!;
    commands.push({
      commandId: `cmd_baseline_${index.toString(16).padStart(4, "0")}`,
      target,
      command: "task/accept",
      arguments: { seed, ordinal: index, target: targetIndex },
      expectedRevision: revision,
      idempotencyKey: `idem_baseline_${index.toString(16).padStart(4, "0")}`,
    });
    revisions[targetIndex] = revision + 1;
  }
  return Object.freeze(commands);
}

function microseconds(start: number, end: number): number {
  return Math.max(0, (end - start) * 1_000);
}

export async function measureAssuranceBaseline(options: Readonly<{
  seed?: number;
  commands?: number;
  targets?: number;
  warmup?: number;
  repetitions?: number;
}> = {}): Promise<BaselineReceipt> {
  const seed = options.seed ?? DEFAULT_SEED;
  const commandCount = options.commands ?? DEFAULT_COMMANDS;
  const targetCount = options.targets ?? DEFAULT_TARGETS;
  const warmup = options.warmup ?? DEFAULT_WARMUP;
  const repetitions = options.repetitions ?? DEFAULT_REPETITIONS;
  const commands = workload(seed, commandCount, targetCount);
  for (let index = 0; index < warmup; index += 1) replayShadowCommands(commands);
  const acceptance: number[] = [];
  const projection: number[] = [];
  const cpu: number[] = [];
  const memory: number[] = [];
  const journal: number[] = [];
  const replay: number[] = [];
  for (let repetition = 0; repetition < repetitions; repetition += 1) {
    const usageBefore = process.resourceUsage();
    const memoryBefore = process.memoryUsage().rss;
    let state = createShadowState();
    const acceptanceStart = performance.now();
    for (const command of commands) {
      const start = performance.now();
      state = applyShadowCommand(state, command).state;
      acceptance.push(microseconds(start, performance.now()));
    }
    const acceptanceEnd = performance.now();
    const encodedState = new TextEncoder().encode(canonicalJson(state)).byteLength;
    const encodedJournal = new TextEncoder().encode(canonicalJson(state.transitions)).byteLength;
    const replayStart = performance.now();
    replayShadowCommands(commands);
    const replayEnd = performance.now();
    const usageAfter = process.resourceUsage();
    const memoryAfter = process.memoryUsage().rss;
    projection.push(encodedState);
    journal.push(encodedJournal);
    replay.push(microseconds(replayStart, replayEnd));
    cpu.push(Math.max(0, (usageAfter.userCPUTime + usageAfter.systemCPUTime) - (usageBefore.userCPUTime + usageBefore.systemCPUTime)));
    memory.push(Math.max(memoryBefore, memoryAfter));
    // Keep a complete-workload sample alive so a caller can distinguish it
    // from the per-command acceptance samples above.
    if (acceptanceEnd < acceptanceStart) throw new Error("monotonic clock moved backwards");
  }
  return {
    schema: "xcb.metrics.baseline-receipt.v1",
    status: "measured",
    generatedAt: new Date().toISOString(),
    workload: { id: "xcb-shadow-acceptance-v1", seed, commands: commandCount, targets: targetCount, warmup, repetitions, manifest: DEFAULT_SHADOW_MANIFEST },
    source: { node: process.version, bun: typeof Bun === "undefined" ? null : Bun.version },
    metrics: {
      acceptanceLatencyUs: summary(acceptance),
      projectionBytes: summary(projection),
      cpuUs: summary(cpu),
      memoryBytes: summary(memory),
      journalGrowthBytes: summary(journal),
      replayTimeUs: summary(replay),
    },
    notes: Object.freeze([
      "Synthetic local shadow-model measurement; no provider, Valhalla peer, or billing claim.",
      "Acceptance latency is per pure command application; replay time is per complete bounded history.",
      "Run the same workload, toolchain, host limits, and source tree for comparisons.",
    ]),
  };
}

if (import.meta.main) {
  const receipt = await measureAssuranceBaseline({
    seed: argument("--seed", DEFAULT_SEED),
    commands: argument("--commands", DEFAULT_COMMANDS),
    targets: argument("--targets", DEFAULT_TARGETS),
    warmup: argument("--warmup", DEFAULT_WARMUP),
    repetitions: argument("--repetitions", DEFAULT_REPETITIONS),
  });
  const outputIndex = process.argv.indexOf("--output");
  const output = outputIndex < 0 ? null : process.argv[outputIndex + 1];
  const text = `${JSON.stringify(receipt, null, 2)}\n`;
  if (output === undefined || output === null) process.stdout.write(text);
  else await writeFile(resolve(output), text, "utf8");
}
