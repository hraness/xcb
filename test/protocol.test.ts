import { readFileSync } from "node:fs";
import { expect, test } from "bun:test";
import {
  XCB_PROTOCOL_FEATURES,
  XCB_PROTOCOL_MAX_FRAME_BYTES,
  XCB_PROTOCOL_SCHEMA,
  createCommandSubmitRequest,
  createCommandSubmitResponse,
  createInitializeRequest,
  createInitializeResponse,
  createProtocolErrorResponse,
  decodeProtocolFrame,
  encodeProtocolFrame,
  negotiateProtocolCapabilities,
} from "../src/protocol.ts";
import type { XcbProtocolCapabilities, XcbProtocolFrame } from "../src/protocol.ts";

type Vector = Readonly<{ name: string; frame: string }>;
type VectorFile = Readonly<{ schema: string; maxFrameBytes: number; golden: readonly Vector[]; negative: readonly Vector[] }>;
const vectors = JSON.parse(readFileSync(new URL("../protocol/v1-vectors.json", import.meta.url), "utf8")) as VectorFile;

function bytes(text: string): Uint8Array {
  return new TextEncoder().encode(text);
}

for (const vector of vectors.golden) {
  test(`protocol golden vector: ${vector.name}`, () => {
    const frame = decodeProtocolFrame(bytes(vector.frame));
    expect(new TextDecoder().decode(encodeProtocolFrame(frame))).toBe(vector.frame);
  });
}

for (const vector of vectors.negative) {
  test(`protocol negative vector: ${vector.name}`, () => {
    expect(() => decodeProtocolFrame(bytes(vector.frame))).toThrow();
  });
}

test("protocol helpers preserve IDs, revisions, and idempotency across a replay response", () => {
  const initialize = createInitializeRequest({ requestId: "req_01", clientName: "test", clientVersion: "1" });
  expect(initialize.expectedRevision).toBeNull();
  expect(initialize.idempotencyKey).toBeNull();
  const command = createCommandSubmitRequest({ requestId: "req_02", command: "task/run",
    arguments: { dryRun: false }, expectedRevision: "rev_abc", idempotencyKey: "idem_01" });
  expect(command.expectedRevision).toBe("rev_abc");
  expect(command.idempotencyKey).toBe("idem_01");
  const replay = createCommandSubmitResponse({ requestId: command.requestId, status: "replayed", receiptId: "rcpt_01", revision: "rev_def" });
  expect((decodeProtocolFrame(encodeProtocolFrame(replay)) as XcbProtocolFrame).kind).toBe("response");
  expect(createInitializeResponse({ requestId: initialize.requestId, capabilities: initialize.capabilities }).ok).toBe(true);
});

test("capability negotiation selects only the current version and offered features", () => {
  const offer: XcbProtocolCapabilities = { versions: [XCB_PROTOCOL_SCHEMA], features: [XCB_PROTOCOL_FEATURES[0]!] };
  expect(negotiateProtocolCapabilities(offer)).toEqual(offer);
  expect(() => negotiateProtocolCapabilities({ versions: ["xcb.protocol.v2"], features: [] })).toThrow("XCB_PROTOCOL_VERSION_UNSUPPORTED");
  expect(() => negotiateProtocolCapabilities({ versions: [XCB_PROTOCOL_SCHEMA], features: ["future.feature"] })).toThrow("XCB_PROTOCOL_CAPABILITY_UNSUPPORTED");
});

test("canonical framing rejects a partial line, duplicate key, and a bounded-frame overflow", () => {
  const initialize = createInitializeRequest({ requestId: "req_01", clientName: "test", clientVersion: "1" });
  const encoded = encodeProtocolFrame(initialize);
  expect(() => decodeProtocolFrame(encoded.subarray(0, encoded.length - 1))).toThrow("XCB_PROTOCOL_FRAME_BOUNDARY");
  expect(() => decodeProtocolFrame(bytes('{"kind":"request","kind":"request"}\n'))).toThrow();
  const tooLarge = new Uint8Array(XCB_PROTOCOL_MAX_FRAME_BYTES + 1);
  tooLarge[tooLarge.length - 1] = 0x0a;
  expect(() => decodeProtocolFrame(tooLarge)).toThrow("XCB_PROTOCOL_FRAME_BOUNDARY");
});

test("error responses carry only a bounded host-selected error", () => {
  const response = createProtocolErrorResponse({ requestId: "req_02", method: "command/submit",
    error: { code: "revision_conflict", message: "stale revision", retryable: true } });
  expect(response.ok).toBe(false);
  expect(new TextDecoder().decode(encodeProtocolFrame(response))).toContain("revision_conflict");
  expect(() => createProtocolErrorResponse({ requestId: "req_02", method: "command/submit",
    error: { code: "internal", message: "\u0000", retryable: false } })).toThrow();
});
