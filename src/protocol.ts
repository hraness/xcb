// @ts-nocheck
import { canonicalJson } from "./canonical-json.ts";
import { boundedText } from "./validation.ts";

/**
 * The first transport-neutral xcb wire slice. It is deliberately not JSON-RPC:
 * transports carry one canonical newline-delimited frame, while request IDs,
 * revisions, and idempotency are part of the xcb message contract.
 */
export const XCB_PROTOCOL_SCHEMA = "xcb.protocol.v1" as const;
export const XCB_PROTOCOL_VERSION = 1 as const;
export const XCB_PROTOCOL_MAX_FRAME_BYTES = 64 * 1024;
export const XCB_PROTOCOL_MAX_ARGUMENT_BYTES = 16 * 1024;
export const XCB_PROTOCOL_MAX_ERROR_BYTES = 512;
export const XCB_PROTOCOL_MAX_CAPABILITIES = 32;
export const XCB_PROTOCOL_MAX_JSON_DEPTH = 12;

export const XCB_PROTOCOL_FEATURES = Object.freeze(["command.submit", "receipt.reference"] as const);
export type XcbProtocolFeature = typeof XCB_PROTOCOL_FEATURES[number];
export type XcbProtocolMethod = "initialize" | "command/submit";
export type XcbProtocolStatus = "accepted" | "replayed";
export type XcbProtocolErrorCode =
  | "invalid_request"
  | "unsupported_version"
  | "unsupported_capability"
  | "revision_conflict"
  | "idempotency_conflict"
  | "not_found"
  | "busy"
  | "internal";

export type ProtocolJson = null | boolean | number | string | readonly ProtocolJson[]
  | Readonly<{ readonly [key: string]: ProtocolJson }>;
export type XcbProtocolCapabilities = Readonly<{
  versions: readonly string[];
  features: readonly string[];
}>;
export type XcbProtocolError = Readonly<{
  code: XcbProtocolErrorCode;
  message: string;
  retryable: boolean;
}>;
export type XcbInitializeParams = Readonly<{
  clientName: string;
  clientVersion: string;
}>;
export type XcbCommandSubmitParams = Readonly<{
  command: string;
  arguments: Readonly<{ readonly [key: string]: ProtocolJson }>;
}>;
export type XcbInitializeRequest = Readonly<{
  schema: typeof XCB_PROTOCOL_SCHEMA;
  kind: "request";
  requestId: string;
  method: "initialize";
  expectedRevision: null;
  idempotencyKey: null;
  capabilities: XcbProtocolCapabilities;
  params: XcbInitializeParams;
}>;
export type XcbCommandSubmitRequest = Readonly<{
  schema: typeof XCB_PROTOCOL_SCHEMA;
  kind: "request";
  requestId: string;
  method: "command/submit";
  expectedRevision: string | null;
  idempotencyKey: string;
  capabilities: null;
  params: XcbCommandSubmitParams;
}>;
export type XcbProtocolRequest = XcbInitializeRequest | XcbCommandSubmitRequest;
export type XcbInitializeResult = Readonly<{
  protocolVersion: typeof XCB_PROTOCOL_SCHEMA;
  capabilities: XcbProtocolCapabilities;
}>;
export type XcbCommandSubmitResult = Readonly<{
  status: XcbProtocolStatus;
  receiptId: string;
  revision: string | null;
}>;
export type XcbProtocolSuccessResponse = Readonly<{
  schema: typeof XCB_PROTOCOL_SCHEMA;
  kind: "response";
  requestId: string;
  method: XcbProtocolMethod;
  ok: true;
  result: XcbInitializeResult | XcbCommandSubmitResult;
  error: null;
}>;
export type XcbProtocolErrorResponse = Readonly<{
  schema: typeof XCB_PROTOCOL_SCHEMA;
  kind: "response";
  requestId: string;
  method: XcbProtocolMethod;
  ok: false;
  result: null;
  error: XcbProtocolError;
}>;
export type XcbProtocolResponse = XcbProtocolSuccessResponse | XcbProtocolErrorResponse;
export type XcbProtocolFrame = XcbProtocolRequest | XcbProtocolResponse;

const REQUEST_KEYS = ["schema", "kind", "requestId", "method", "expectedRevision", "idempotencyKey", "capabilities", "params"] as const;
const RESPONSE_KEYS = ["schema", "kind", "requestId", "method", "ok", "result", "error"] as const;
const ERROR_KEYS = ["code", "message", "retryable"] as const;
const FEATURE_SET = new Set<string>(XCB_PROTOCOL_FEATURES);
const ERROR_CODES = new Set<XcbProtocolErrorCode>([
  "invalid_request", "unsupported_version", "unsupported_capability", "revision_conflict",
  "idempotency_conflict", "not_found", "busy", "internal",
]);
const METHODS = new Set<XcbProtocolMethod>(["initialize", "command/submit"]);
const fail = (code: string): never => { throw new Error(`XCB_PROTOCOL_${code}`); };
const isRecord = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const isPlainRecord = (value: unknown): value is Record<string, unknown> =>
  isRecord(value) && (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null);

function exactRecord(value: unknown, keys: readonly string[], code: string): Record<string, any> {
  if (!isPlainRecord(value)) fail(code);
  const own = Object.keys(value);
  if (own.length !== keys.length || own.some(key => !keys.includes(key))) fail(code);
  return value;
}

function unicode(value: string, code: string): void {
  for (let index = 0; index < value.length; index += 1) {
    const unit = value.charCodeAt(index);
    if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) fail(code);
      index += 1;
    } else if (unit >= 0xdc00 && unit <= 0xdfff) fail(code);
  }
}

function text(value: unknown, maxBytes: number, code: string, empty = false): string {
  let result!: string;
  try { result = boundedText(value, maxBytes, empty); } catch { fail(code); }
  unicode(result, code);
  return result;
}

function protocolText(value: unknown, maxBytes: number, code: string, empty = false): string {
  const result = text(value, maxBytes, code, empty);
  if (/\p{C}/u.test(result)) fail(code);
  return result;
}

function token(value: unknown, prefix: string, maxBytes: number, code: string): string {
  const result = protocolText(value, maxBytes, code);
  if (result.length <= prefix.length || !result.startsWith(prefix) || !/^[A-Za-z0-9][A-Za-z0-9_.:-]*$/u.test(result)) fail(code);
  return result;
}

function revision(value: unknown, code: string): string | null {
  if (value === null) return null;
  const result = protocolText(value, 160, code);
  if (!/^[A-Za-z0-9][A-Za-z0-9_.:-]*$/u.test(result)) fail(code);
  return result;
}

function sortedUnique(values: readonly string[], code: string): void {
  for (let index = 1; index < values.length; index += 1) {
    if (values[index - 1]! >= values[index]!) fail(code);
  }
}

function validateCapabilities(value: unknown, code = "CAPABILITIES_INVALID"): XcbProtocolCapabilities {
  const fields = exactRecord(value, ["versions", "features"], code);
  if (!Array.isArray(fields.versions) || !Array.isArray(fields.features)
    || fields.versions.length === 0 || fields.versions.length > 8
    || fields.features.length > XCB_PROTOCOL_MAX_CAPABILITIES) fail(code);
  const versions = fields.versions.map(item => protocolText(item, 96, code));
  const features = fields.features.map(item => protocolText(item, 96, code));
  if (new Set(versions).size !== versions.length || new Set(features).size !== features.length) fail(code);
  sortedUnique(versions, code); sortedUnique(features, code);
  if (!versions.includes(XCB_PROTOCOL_SCHEMA)) fail("VERSION_UNSUPPORTED");
  if (features.some(feature => !FEATURE_SET.has(feature))) fail("CAPABILITY_UNSUPPORTED");
  return Object.freeze({ versions: Object.freeze(versions), features: Object.freeze(features) });
}

function validateJson(value: unknown, depth = 0, ancestors = new Set<object>()): asserts value is ProtocolJson {
  if (depth > XCB_PROTOCOL_MAX_JSON_DEPTH) fail("ARGUMENT_DEPTH");
  if (value === null || typeof value === "boolean") return;
  if (typeof value === "string") { unicode(value, "ARGUMENT_UNICODE"); return; }
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value) || Object.is(value, -0)) fail("ARGUMENT_NUMBER");
    return;
  }
  if (typeof value !== "object") fail("ARGUMENT_VALUE");
  if (ancestors.has(value)) fail("ARGUMENT_CYCLE");
  ancestors.add(value);
  try {
    if (Array.isArray(value)) {
      if (value.length > 128) fail("ARGUMENT_ARRAY");
      value.forEach(item => validateJson(item, depth + 1, ancestors));
      return;
    }
    if (!isPlainRecord(value)) fail("ARGUMENT_OBJECT");
    const keys = Object.keys(value);
    if (keys.length > 128) fail("ARGUMENT_OBJECT");
    for (const key of keys) {
      unicode(key, "ARGUMENT_KEY");
      if (new TextEncoder().encode(key).byteLength > 128 || /\p{C}/u.test(key)) fail("ARGUMENT_KEY");
      validateJson(value[key], depth + 1, ancestors);
    }
  } finally { ancestors.delete(value); }
}

function validateArguments(value: unknown): Readonly<{ readonly [key: string]: ProtocolJson }> {
  if (!isPlainRecord(value)) fail("ARGUMENTS_INVALID");
  validateJson(value);
  let bytes!: number;
  try { bytes = new TextEncoder().encode(canonicalJson(value)).byteLength; } catch { fail("ARGUMENTS_INVALID"); }
  if (bytes > XCB_PROTOCOL_MAX_ARGUMENT_BYTES) fail("ARGUMENTS_LIMIT");
  return value as Readonly<{ readonly [key: string]: ProtocolJson }>;
}

function validateRequest(value: unknown): XcbProtocolRequest {
  const fields = exactRecord(value, REQUEST_KEYS, "REQUEST_SHAPE");
  if (fields.schema !== XCB_PROTOCOL_SCHEMA) fail("SCHEMA_INVALID");
  if (fields.kind !== "request") fail("KIND_INVALID");
  const requestId = token(fields.requestId, "req_", 128, "REQUEST_ID_INVALID");
  const expectedRevision = revision(fields.expectedRevision, "REVISION_INVALID");
  const idempotencyKey = fields.idempotencyKey === null
    ? null : token(fields.idempotencyKey, "idem_", 160, "IDEMPOTENCY_INVALID");
  if (fields.method === "initialize") {
    if (expectedRevision !== null || idempotencyKey !== null || fields.capabilities === null) fail("INITIALIZE_METADATA");
    const capabilities = validateCapabilities(fields.capabilities);
    const params = exactRecord(fields.params, ["clientName", "clientVersion"], "INITIALIZE_PARAMS");
    return Object.freeze({ schema: XCB_PROTOCOL_SCHEMA, kind: "request", requestId, method: "initialize",
      expectedRevision: null, idempotencyKey: null, capabilities,
      params: Object.freeze({ clientName: protocolText(params.clientName, 128, "CLIENT_NAME_INVALID"), clientVersion: protocolText(params.clientVersion, 64, "CLIENT_VERSION_INVALID") }) });
  }
  if (fields.method !== "command/submit") fail("METHOD_INVALID");
  if (idempotencyKey === null) fail("IDEMPOTENCY_REQUIRED");
  if (fields.capabilities !== null) fail("COMMAND_CAPABILITIES");
  const params = exactRecord(fields.params, ["command", "arguments"], "COMMAND_PARAMS");
  const command = protocolText(params.command, 96, "COMMAND_INVALID");
  if (!/^[A-Za-z][A-Za-z0-9._/-]*$/u.test(command)) fail("COMMAND_INVALID");
  return Object.freeze({ schema: XCB_PROTOCOL_SCHEMA, kind: "request", requestId, method: "command/submit",
    expectedRevision, idempotencyKey, capabilities: null,
    params: Object.freeze({ command, arguments: validateArguments(params.arguments) }) });
}

function validateError(value: unknown): XcbProtocolError {
  const fields = exactRecord(value, ERROR_KEYS, "ERROR_SHAPE");
  const code = fields.code;
  if (typeof code !== "string" || !ERROR_CODES.has(code as XcbProtocolErrorCode)) fail("ERROR_CODE");
  const message = protocolText(fields.message, XCB_PROTOCOL_MAX_ERROR_BYTES, "ERROR_MESSAGE_INVALID", true);
  if (message.length === 0) fail("ERROR_MESSAGE_INVALID");
  if (typeof fields.retryable !== "boolean") fail("ERROR_RETRYABLE");
  if (new TextEncoder().encode(message).byteLength > XCB_PROTOCOL_MAX_ERROR_BYTES) fail("ERROR_LIMIT");
  return Object.freeze({ code: code as XcbProtocolErrorCode, message, retryable: fields.retryable });
}

function validateResult(method: XcbProtocolMethod, value: unknown): XcbInitializeResult | XcbCommandSubmitResult {
  if (method === "initialize") {
    const fields = exactRecord(value, ["protocolVersion", "capabilities"], "INITIALIZE_RESULT");
    if (fields.protocolVersion !== XCB_PROTOCOL_SCHEMA) fail("VERSION_INVALID");
    return Object.freeze({ protocolVersion: XCB_PROTOCOL_SCHEMA, capabilities: validateCapabilities(fields.capabilities) });
  }
  const fields = exactRecord(value, ["status", "receiptId", "revision"], "COMMAND_RESULT");
  const status = fields.status;
  if (status !== "accepted" && status !== "replayed") fail("STATUS_INVALID");
  const receiptId = token(fields.receiptId, "rcpt_", 160, "RECEIPT_ID_INVALID");
  return Object.freeze({ status: status as XcbProtocolStatus, receiptId, revision: revision(fields.revision, "REVISION_INVALID") });
}

function validateResponse(value: unknown): XcbProtocolResponse {
  const fields = exactRecord(value, RESPONSE_KEYS, "RESPONSE_SHAPE");
  if (fields.schema !== XCB_PROTOCOL_SCHEMA) fail("SCHEMA_INVALID");
  if (fields.kind !== "response") fail("KIND_INVALID");
  const requestId = token(fields.requestId, "req_", 128, "REQUEST_ID_INVALID");
  if (typeof fields.method !== "string" || !METHODS.has(fields.method as XcbProtocolMethod)) fail("METHOD_INVALID");
  const method = fields.method as XcbProtocolMethod;
  if (typeof fields.ok !== "boolean") fail("OK_INVALID");
  if (fields.ok) {
    if (fields.error !== null) fail("SUCCESS_ERROR");
    return Object.freeze({ schema: XCB_PROTOCOL_SCHEMA, kind: "response", requestId, method, ok: true,
      result: validateResult(method, fields.result), error: null });
  }
  if (fields.result !== null) fail("ERROR_RESULT");
  return Object.freeze({ schema: XCB_PROTOCOL_SCHEMA, kind: "response", requestId, method, ok: false,
    result: null, error: validateError(fields.error) });
}

/** Validate a decoded JSON value without selecting a transport. */
export function validateProtocolFrame(value: unknown): XcbProtocolFrame {
  if (!isPlainRecord(value) || typeof value.kind !== "string") fail("FRAME_SHAPE");
  return value.kind === "request" ? validateRequest(value) : value.kind === "response" ? validateResponse(value) : fail("KIND_INVALID");
}

/**
 * Encode one frame as canonical UTF-8 JSON followed by exactly one LF. The LF
 * is the only framing convention; no socket, stdio, or Valhalla transport is
 * selected here.
 */
export function encodeProtocolFrame(value: XcbProtocolFrame): Uint8Array {
  const frame = validateProtocolFrame(value);
  let canonical!: string;
  try { canonical = canonicalJson(frame); } catch { fail("CANONICAL_JSON"); }
  const bytes = new TextEncoder().encode(`${canonical}\n`);
  if (bytes.byteLength > XCB_PROTOCOL_MAX_FRAME_BYTES) fail("FRAME_LIMIT");
  return bytes;
}

/** Decode one canonical frame; non-canonical JSON, duplicate keys, batches,
 * partial lines, and trailing bytes are rejected before schema dispatch. */
export function decodeProtocolFrame(input: Uint8Array): XcbProtocolFrame {
  if (!(input instanceof Uint8Array)) fail("FRAME_BYTES");
  if (input.byteLength === 0 || input.byteLength > XCB_PROTOCOL_MAX_FRAME_BYTES || input[input.byteLength - 1] !== 0x0a) fail("FRAME_BOUNDARY");
  for (let index = 0; index < input.byteLength - 1; index += 1) {
    if (input[index] === 0x0a || input[index] === 0x0d) fail("FRAME_BOUNDARY");
  }
  let textValue!: string;
  try { textValue = new TextDecoder("utf-8", { fatal: true }).decode(input.subarray(0, -1)); } catch { fail("FRAME_ENCODING"); }
  if (textValue.length === 0) fail("FRAME_JSON");
  let value: unknown;
  try { value = JSON.parse(textValue) as unknown; } catch { fail("FRAME_JSON"); }
  let canonical!: string;
  try { canonical = canonicalJson(value); } catch { fail("CANONICAL_JSON"); }
  if (canonical !== textValue) fail("FRAME_NOT_CANONICAL");
  return validateProtocolFrame(value);
}

export function negotiateProtocolCapabilities(value: unknown): XcbProtocolCapabilities {
  const offered = validateCapabilities(value, "CAPABILITIES_INVALID");
  if (!offered.versions.includes(XCB_PROTOCOL_SCHEMA)) fail("VERSION_UNSUPPORTED");
  return Object.freeze({ versions: Object.freeze([XCB_PROTOCOL_SCHEMA]),
    features: Object.freeze(XCB_PROTOCOL_FEATURES.filter(feature => offered.features.includes(feature))) });
}

export function createInitializeRequest(input: Readonly<{
  requestId: string;
  clientName: string;
  clientVersion: string;
  capabilities?: XcbProtocolCapabilities;
}>): XcbInitializeRequest {
  const value = {
    schema: XCB_PROTOCOL_SCHEMA, kind: "request" as const, requestId: input.requestId, method: "initialize" as const,
    expectedRevision: null, idempotencyKey: null, capabilities: input.capabilities ?? {
      versions: [XCB_PROTOCOL_SCHEMA], features: [...XCB_PROTOCOL_FEATURES],
    }, params: { clientName: input.clientName, clientVersion: input.clientVersion },
  };
  return validateRequest(value) as XcbInitializeRequest;
}

export function createCommandSubmitRequest(input: Readonly<{
  requestId: string;
  command: string;
  arguments: Readonly<{ readonly [key: string]: ProtocolJson }>;
  expectedRevision: string | null;
  idempotencyKey: string;
}>): XcbCommandSubmitRequest {
  return validateRequest({
    schema: XCB_PROTOCOL_SCHEMA, kind: "request", requestId: input.requestId, method: "command/submit",
    expectedRevision: input.expectedRevision, idempotencyKey: input.idempotencyKey, capabilities: null,
    params: { command: input.command, arguments: input.arguments },
  }) as XcbCommandSubmitRequest;
}

export function createInitializeResponse(input: Readonly<{ requestId: string; capabilities: XcbProtocolCapabilities }>): XcbProtocolSuccessResponse {
  return validateResponse({ schema: XCB_PROTOCOL_SCHEMA, kind: "response", requestId: input.requestId, method: "initialize", ok: true,
    result: { protocolVersion: XCB_PROTOCOL_SCHEMA, capabilities: negotiateProtocolCapabilities(input.capabilities) }, error: null }) as XcbProtocolSuccessResponse;
}

export function createCommandSubmitResponse(input: Readonly<{
  requestId: string;
  status: XcbProtocolStatus;
  receiptId: string;
  revision: string | null;
}>): XcbProtocolSuccessResponse {
  return validateResponse({ schema: XCB_PROTOCOL_SCHEMA, kind: "response", requestId: input.requestId, method: "command/submit", ok: true,
    result: { status: input.status, receiptId: input.receiptId, revision: input.revision }, error: null }) as XcbProtocolSuccessResponse;
}

export function createProtocolErrorResponse(input: Readonly<{
  requestId: string;
  method: XcbProtocolMethod;
  error: XcbProtocolError;
}>): XcbProtocolErrorResponse {
  return validateResponse({ schema: XCB_PROTOCOL_SCHEMA, kind: "response", requestId: input.requestId, method: input.method,
    ok: false, result: null, error: input.error }) as XcbProtocolErrorResponse;
}
