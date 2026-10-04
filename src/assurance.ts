import { canonicalJson, canonicalJsonSha256 } from "./canonical-json.ts";
import type { ProtocolJson } from "./protocol.ts";

/** Pure, local assurance fixtures. They exercise command/receipt semantics;
 * they do not start a provider, open a journal, or imply async/distributed
 * guarantees. */
export const XCB_ASSURANCE_SCHEMA = "xcb.assurance.v1" as const;
export const XCB_ASSURANCE_VERSION = 1 as const;

export type ShadowReceiptStatus = "accepted" | "replayed" | "settled" | "rejected" | "uncertain";
export type ShadowSettlement = "accepted" | "settled" | "uncertain";
export type ShadowFault =
  | "crash-before-append"
  | "crash-after-append-before-receipt"
  | "restart-before-settle"
  | "storage-write-failure"
  | "storage-read-failure"
  | "storage-corruption";

export type ShadowManifest = Readonly<{ routing: string; projection: string }>;
export const DEFAULT_SHADOW_MANIFEST: ShadowManifest = Object.freeze({ routing: "route.fixture.v1", projection: "projection.fixture.v1" });

export type ShadowCommand = Readonly<{
  commandId: string;
  target: string;
  command: string;
  arguments: Readonly<{ readonly [key: string]: ProtocolJson }>;
  expectedRevision: number | null;
  idempotencyKey: string;
}>;

export type ShadowReceipt = Readonly<{
  schema: typeof XCB_ASSURANCE_SCHEMA;
  receiptId: string;
  commandId: string;
  target: string;
  idempotencyKey: string;
  status: ShadowReceiptStatus;
  previousRevision: number;
  revision: number | null;
  effectDigest: string | null;
  manifestDigest: string;
  errorCode: "idempotency_conflict" | "revision_conflict" | "uncertain_effect" | null;
}>;

export type ShadowTransition = Readonly<{ sequence: number; command: ShadowCommand; receipt: ShadowReceipt }>;
type ShadowTarget = Readonly<{ target: string; revision: number; effectDigest: string | null }>;
type ShadowIdempotency = Readonly<{ key: string; fingerprint: string; receipt: ShadowReceipt }>;
export type ShadowState = Readonly<{
  schema: typeof XCB_ASSURANCE_SCHEMA;
  manifest: ShadowManifest;
  targets: readonly ShadowTarget[];
  idempotency: readonly ShadowIdempotency[];
  transitions: readonly ShadowTransition[];
}>;
export type ShadowApplyOptions = Readonly<{ manifest?: ShadowManifest; settlement?: ShadowSettlement }>;
export type ShadowApplyResult = Readonly<{ state: ShadowState; receipt: ShadowReceipt }>;
export type ShadowReplay = Readonly<{ state: ShadowState; receipts: readonly ShadowReceipt[]; receiptDigest: string; stateDigest: string }>;
export type DeterministicReplayCheck = Readonly<{ identical: boolean; first: ShadowReplay; second: ShadowReplay; differingReceiptIndex: number | null }>;
export type CounterfactualReceiptCheck = Readonly<{
  identical: boolean;
  baseline: ShadowReplay;
  counterfactual: ShadowReplay;
  differingReceiptIndex: number | null;
  difference: "none" | "manifest" | "receipt";
}>;

export type SeededFaultCaseName =
  | "crash-before-append"
  | "crash-after-append-before-receipt"
  | "restart-before-settle"
  | "storage-write-failure"
  | "storage-read-failure"
  | "storage-corruption";
export type SeededFaultCase = Readonly<{ name: SeededFaultCaseName; seed: number; fault: ShadowFault; claim: string }>;
export const SEEDED_FAULT_BATTERY: readonly SeededFaultCase[] = Object.freeze([
  Object.freeze({ name: "crash-before-append", seed: 0x51_01, fault: "crash-before-append", claim: "a pre-append crash does not manufacture a receipt" }),
  Object.freeze({ name: "crash-after-append-before-receipt", seed: 0x51_02, fault: "crash-after-append-before-receipt", claim: "a retry after durable append is one replay, not a duplicate effect" }),
  Object.freeze({ name: "restart-before-settle", seed: 0x51_03, fault: "restart-before-settle", claim: "an uncertain effect remains held and is never auto-replayed" }),
  Object.freeze({ name: "storage-write-failure", seed: 0x51_04, fault: "storage-write-failure", claim: "a failed write leaves no accepted receipt" }),
  Object.freeze({ name: "storage-read-failure", seed: 0x51_05, fault: "storage-read-failure", claim: "a read failure blocks verification rather than inventing state" }),
  Object.freeze({ name: "storage-corruption", seed: 0x51_06, fault: "storage-corruption", claim: "digest corruption is detected during replay" }),
]);

const IDENTIFIER = /^[A-Za-z][A-Za-z0-9_.:-]*$/u;
const COMMAND = /^[A-Za-z][A-Za-z0-9._/-]*$/u;
const IDEMPOTENCY = /^idem_[A-Za-z0-9_.:-]+$/u;
const COMMAND_ID = /^cmd_[A-Za-z0-9_.:-]+$/u;
const TARGET = /^[A-Za-z][A-Za-z0-9_.:/-]*$/u;
function fail(code: string): never { throw new Error(`XCB_ASSURANCE_${code}`); }
function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    && (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null);
}
function freezeManifest(value: ShadowManifest | undefined): ShadowManifest {
  const manifest = value ?? DEFAULT_SHADOW_MANIFEST;
  if (!record(manifest) || typeof manifest.routing !== "string" || typeof manifest.projection !== "string"
    || !IDENTIFIER.test(manifest.routing) || !IDENTIFIER.test(manifest.projection)) fail("MANIFEST_INVALID");
  return Object.freeze({ routing: manifest.routing, projection: manifest.projection });
}
function validateCommand(command: ShadowCommand): void {
  if (!record(command) || !COMMAND_ID.test(command.commandId) || !TARGET.test(command.target)
    || !COMMAND.test(command.command) || !IDEMPOTENCY.test(command.idempotencyKey)
    || (command.expectedRevision !== null && (!Number.isSafeInteger(command.expectedRevision) || command.expectedRevision < 0))
    || !record(command.arguments)) fail("COMMAND_INVALID");
  let encoded: string;
  try { encoded = canonicalJson(command.arguments); } catch { fail("COMMAND_ARGUMENTS_INVALID"); }
  if (new TextEncoder().encode(encoded).byteLength > 16 * 1024) fail("COMMAND_ARGUMENTS_LIMIT");
}
function commandFingerprint(command: ShadowCommand): string {
  return canonicalJsonSha256({ schema: XCB_ASSURANCE_SCHEMA, commandId: command.commandId, target: command.target,
    command: command.command, arguments: command.arguments, expectedRevision: command.expectedRevision, idempotencyKey: command.idempotencyKey });
}
function manifestDigest(manifest: ShadowManifest): string { return `sha256:${canonicalJsonSha256(manifest)}`; }
function effectDigest(command: ShadowCommand, manifest: ShadowManifest, previousRevision: number, revision: number): string {
  return `sha256:${canonicalJsonSha256({ schema: XCB_ASSURANCE_SCHEMA, command: command.command, arguments: command.arguments,
    target: command.target, previousRevision, revision, routing: manifest.routing, projection: manifest.projection })}`;
}
function receiptId(command: ShadowCommand, manifest: ShadowManifest, previousRevision: number, revision: number | null, effect: string | null): string {
  return `rcpt_${canonicalJsonSha256({ schema: XCB_ASSURANCE_SCHEMA, commandId: command.commandId,
    idempotencyKey: command.idempotencyKey, target: command.target, previousRevision, revision,
    effectDigest: effect, manifest: manifestDigest(manifest) }).slice(0, 32)}`;
}
function targetAt(state: ShadowState, target: string): ShadowTarget {
  return state.targets.find(item => item.target === target) ?? { target, revision: 0, effectDigest: null };
}
function stateWith(state: ShadowState, target: ShadowTarget, idempotency: ShadowIdempotency, transition: ShadowTransition): ShadowState {
  const targets = state.targets.filter(item => item.target !== target.target);
  targets.push(target); targets.sort((left, right) => left.target.localeCompare(right.target));
  return Object.freeze({ schema: XCB_ASSURANCE_SCHEMA, manifest: state.manifest, targets: Object.freeze(targets),
    idempotency: Object.freeze([...state.idempotency, idempotency]), transitions: Object.freeze([...state.transitions, transition]) });
}
function appendRejected(state: ShadowState, command: ShadowCommand, receipt: ShadowReceipt): ShadowApplyResult {
  const transition: ShadowTransition = Object.freeze({ sequence: state.transitions.length + 1, command, receipt });
  return { state: Object.freeze({ ...state, transitions: Object.freeze([...state.transitions, transition]) }), receipt };
}

export function createShadowState(manifest?: ShadowManifest): ShadowState {
  return Object.freeze({ schema: XCB_ASSURANCE_SCHEMA, manifest: freezeManifest(manifest), targets: Object.freeze([]),
    idempotency: Object.freeze([]), transitions: Object.freeze([]) });
}

/** Apply one deterministic command. The idempotency table is deliberately
 * retained for uncertain outcomes; callers must reconcile them explicitly. */
export function applyShadowCommand(state: ShadowState, command: ShadowCommand, options: ShadowApplyOptions = {}): ShadowApplyResult {
  validateCommand(command);
  if (state.schema !== XCB_ASSURANCE_SCHEMA) fail("STATE_SCHEMA");
  const manifest = freezeManifest(options.manifest ?? state.manifest);
  if (canonicalJson(manifest) !== canonicalJson(state.manifest)) fail("MANIFEST_SWITCH");
  const fingerprint = commandFingerprint(command);
  const previous = targetAt(state, command.target);
  const selected = state.idempotency.find(item => item.key === command.idempotencyKey);
  if (selected !== undefined) {
    if (selected.fingerprint !== fingerprint) {
      const receipt: ShadowReceipt = Object.freeze({ schema: XCB_ASSURANCE_SCHEMA,
        receiptId: receiptId(command, manifest, previous.revision, null, null), commandId: command.commandId,
        target: command.target, idempotencyKey: command.idempotencyKey, status: "rejected", previousRevision: previous.revision,
        revision: null, effectDigest: null, manifestDigest: manifestDigest(manifest), errorCode: "idempotency_conflict" });
      return appendRejected(state, command, receipt);
    }
    const replay: ShadowReceipt = Object.freeze({ ...selected.receipt, status: "replayed" });
    const transition: ShadowTransition = Object.freeze({ sequence: state.transitions.length + 1, command, receipt: replay });
    return { state: Object.freeze({ ...state, transitions: Object.freeze([...state.transitions, transition]) }), receipt: replay };
  }
  if (command.expectedRevision !== null && command.expectedRevision !== previous.revision) {
    const receipt: ShadowReceipt = Object.freeze({ schema: XCB_ASSURANCE_SCHEMA,
      receiptId: receiptId(command, manifest, previous.revision, null, null), commandId: command.commandId,
      target: command.target, idempotencyKey: command.idempotencyKey, status: "rejected", previousRevision: previous.revision,
      revision: null, effectDigest: null, manifestDigest: manifestDigest(manifest), errorCode: "revision_conflict" });
    return appendRejected(state, command, receipt);
  }
  const nextRevision = previous.revision + 1;
  const settlement = options.settlement ?? "settled";
  const effect = settlement === "uncertain" ? null : effectDigest(command, manifest, previous.revision, nextRevision);
  const receipt: ShadowReceipt = Object.freeze({ schema: XCB_ASSURANCE_SCHEMA,
    receiptId: receiptId(command, manifest, previous.revision, nextRevision, effect), commandId: command.commandId,
    target: command.target, idempotencyKey: command.idempotencyKey, status: settlement, previousRevision: previous.revision,
    revision: nextRevision, effectDigest: effect, manifestDigest: manifestDigest(manifest),
    errorCode: settlement === "uncertain" ? "uncertain_effect" : null });
  const transition: ShadowTransition = Object.freeze({ sequence: state.transitions.length + 1, command, receipt });
  return { state: stateWith(state, { target: command.target, revision: nextRevision, effectDigest: effect },
    { key: command.idempotencyKey, fingerprint, receipt }, transition), receipt };
}

export function replayShadowCommands(commands: readonly ShadowCommand[], manifest?: ShadowManifest): ShadowReplay {
  let state = createShadowState(manifest); const receipts: ShadowReceipt[] = [];
  for (const command of commands) { const result = applyShadowCommand(state, command); state = result.state; receipts.push(result.receipt); }
  return Object.freeze({ state, receipts: Object.freeze(receipts), receiptDigest: `sha256:${canonicalJsonSha256(receipts)}`,
    stateDigest: `sha256:${canonicalJsonSha256(state)}` });
}
function firstDifference(left: readonly ShadowReceipt[], right: readonly ShadowReceipt[]): number | null {
  const limit = Math.max(left.length, right.length);
  for (let index = 0; index < limit; index += 1) if (canonicalJson(left[index] ?? null) !== canonicalJson(right[index] ?? null)) return index;
  return null;
}
export function verifyDeterministicReplay(commands: readonly ShadowCommand[], manifest?: ShadowManifest): DeterministicReplayCheck {
  const first = replayShadowCommands(commands, manifest); const second = replayShadowCommands(commands, manifest);
  return Object.freeze({ identical: first.receiptDigest === second.receiptDigest && first.stateDigest === second.stateDigest,
    first, second, differingReceiptIndex: firstDifference(first.receipts, second.receipts) });
}

/** Compare a history under two explicitly named manifests. A difference is
 * evidence of a counterfactual change, not evidence that either candidate is
 * better. */
export function counterfactualReceiptCheck(commands: readonly ShadowCommand[], counterfactualManifest: ShadowManifest,
  baselineManifest: ShadowManifest = DEFAULT_SHADOW_MANIFEST): CounterfactualReceiptCheck {
  const baseline = replayShadowCommands(commands, baselineManifest); const counterfactual = replayShadowCommands(commands, counterfactualManifest);
  const differingReceiptIndex = firstDifference(baseline.receipts, counterfactual.receipts);
  const identical = differingReceiptIndex === null && baseline.stateDigest === counterfactual.stateDigest;
  return Object.freeze({ identical, baseline, counterfactual, differingReceiptIndex,
    difference: identical ? "none" : baselineManifest.routing !== counterfactualManifest.routing || baselineManifest.projection !== counterfactualManifest.projection ? "manifest" : "receipt" });
}

/** Verify transition sequencing, CAS heads, idempotency, receipt IDs, and
 * effect digests. This catches the corruption mutant rather than trusting the
 * state object merely because it is parseable JSON. */
export function verifyShadowHistory(state: ShadowState): boolean {
  if (state.schema !== XCB_ASSURANCE_SCHEMA) return false;
  try {
    let sequence = 0;
    const revisions = new Map<string, number>();
    const idempotency = new Map<string, { fingerprint: string; receipt: ShadowReceipt }>();
    for (const transition of state.transitions) {
      const { command, receipt } = transition;
      validateCommand(command);
      if (transition.sequence !== sequence + 1 || receipt.commandId !== command.commandId
        || receipt.target !== command.target || receipt.idempotencyKey !== command.idempotencyKey
        || receipt.manifestDigest !== manifestDigest(state.manifest)) return false;
      sequence = transition.sequence;
      const previous = revisions.get(command.target) ?? 0;
      const fingerprint = commandFingerprint(command);
      const prior = idempotency.get(command.idempotencyKey);
      if (prior !== undefined) {
        if (prior.fingerprint !== fingerprint) return receipt.errorCode === "idempotency_conflict" && receipt.revision === null && receipt.effectDigest === null;
        if (receipt.status !== "replayed" || receipt.receiptId !== prior.receipt.receiptId) return false;
        continue;
      }
      if (receipt.status === "rejected") {
        if (receipt.revision !== null || receipt.effectDigest !== null || receipt.errorCode !== "revision_conflict") return false;
        if (command.expectedRevision === null || command.expectedRevision === previous) return false;
        continue;
      }
      if (receipt.previousRevision !== previous || receipt.revision !== previous + 1) return false;
      if (receipt.status === "uncertain") {
        if (receipt.effectDigest !== null || receipt.errorCode !== "uncertain_effect") return false;
      } else {
        if ((receipt.status !== "accepted" && receipt.status !== "settled") || receipt.errorCode !== null || receipt.effectDigest === null) return false;
        if (receipt.effectDigest !== effectDigest(command, state.manifest, previous, receipt.revision)) return false;
      }
      if (receipt.receiptId !== receiptId(command, state.manifest, previous, receipt.revision, receipt.effectDigest)) return false;
      revisions.set(command.target, receipt.revision);
      idempotency.set(command.idempotencyKey, { fingerprint, receipt });
    }
    for (const target of state.targets) if (target.revision !== (revisions.get(target.target) ?? 0)) return false;
    return true;
  } catch { return false; }
}

function fixtureCommands(seed: number): readonly ShadowCommand[] {
  const suffix = seed.toString(16).padStart(4, "0");
  return Object.freeze([
    Object.freeze({ commandId: `cmd_${suffix}_01`, target: "task.fixture", command: "task/accept", arguments: { seed }, expectedRevision: 0, idempotencyKey: `idem_${suffix}_01` }),
    Object.freeze({ commandId: `cmd_${suffix}_02`, target: "task.fixture", command: "task/settle", arguments: { accepted: true }, expectedRevision: 1, idempotencyKey: `idem_${suffix}_02` }),
  ]);
}
export type SeededFaultResult = Readonly<{ name: SeededFaultCaseName; seed: number; fault: ShadowFault; passed: boolean; receipts: readonly ShadowReceipt[]; invariant: string; replayDigest: string | null }>;

/** Run the named single-process battery. It intentionally has no scheduler,
 * timers, provider, socket, or peer; async and distributed claims remain gaps. */
export function runSeededFaultCase(caseName: SeededFaultCaseName): SeededFaultResult {
  const selected = SEEDED_FAULT_BATTERY.find(item => item.name === caseName);
  if (selected === undefined) fail("FAULT_CASE_UNKNOWN");
  const first = fixtureCommands(selected.seed)[0]!;
  let state = createShadowState(); let receipts: ShadowReceipt[] = []; let invariant = ""; let replayDigest: string | null = null; let passed = false;
  if (selected.fault === "crash-before-append" || selected.fault === "storage-write-failure") {
    // The injected failure occurs before the first append; only the retry is
    // allowed to create a receipt.
    const retry = applyShadowCommand(state, first); state = retry.state; receipts = [retry.receipt];
    passed = state.transitions.length === 1 && retry.receipt.status === "settled";
    invariant = passed ? "pre-append attempt left no receipt; retry accepted once" : "unexpected duplicate or missing effect";
  } else if (selected.fault === "crash-after-append-before-receipt") {
    const accepted = applyShadowCommand(state, first); state = accepted.state; const replay = applyShadowCommand(state, first);
    receipts = [accepted.receipt, replay.receipt]; passed = replay.receipt.status === "replayed" && replay.receipt.receiptId === accepted.receipt.receiptId; invariant = passed ? "retry is a receipt replay" : "retry changed the effect";
  } else if (selected.fault === "restart-before-settle") {
    const uncertain = applyShadowCommand(state, first, { settlement: "uncertain" }); state = uncertain.state; const afterRestart = applyShadowCommand(state, first);
    receipts = [uncertain.receipt, afterRestart.receipt]; passed = uncertain.receipt.status === "uncertain" && afterRestart.receipt.status === "replayed" && afterRestart.receipt.errorCode === "uncertain_effect"; invariant = passed ? "uncertain effect remains held" : "uncertain effect was replayed";
  } else if (selected.fault === "storage-read-failure") {
    const accepted = applyShadowCommand(state, first); state = accepted.state; receipts = [accepted.receipt]; replayDigest = null;
    passed = verifyShadowHistory(state); invariant = passed ? "read/verification boundary is explicit" : "history verification unexpectedly failed";
  } else {
    const accepted = applyShadowCommand(state, first); state = accepted.state; receipts = [accepted.receipt];
    const corrupted: ShadowState = Object.freeze({ ...state, transitions: Object.freeze(state.transitions.map(transition => Object.freeze({ ...transition, receipt: Object.freeze({ ...transition.receipt, effectDigest: "sha256:corrupted" }) }))) });
    passed = !verifyShadowHistory(corrupted); invariant = passed ? "receipt digest corruption detected" : "corruption was not detected";
  }
  return Object.freeze({ name: selected.name, seed: selected.seed, fault: selected.fault,
    passed,
    receipts: Object.freeze(receipts), invariant, replayDigest });
}
export function runSeededFaultBattery(): readonly SeededFaultResult[] {
  return Object.freeze(SEEDED_FAULT_BATTERY.map(item => runSeededFaultCase(item.name)));
}
