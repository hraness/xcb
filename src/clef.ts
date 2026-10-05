import { inflateSync } from "node:zlib";
import { checkJudgeAnswers, checkJudgeQuestions, checkJudgeState, parseJudgeResponse,
  type Judge, type JudgeAnswers, type JudgeQuestions, type JudgeState, type JudgeAskOptions } from "./judge.ts";

export const CLEF_MODELS = ["clef", "clef-flash"] as const;
export type ClefModel = typeof CLEF_MODELS[number];
export type ClefImage = string | { content_type: "image/png" | "image/jpeg" | "image/webp"; base64: string };
export const CLEF_ACCOUNT_ENV = "CLOUDFLARE_ACCOUNT_ID";
export const CLEF_TOKEN_ENV = "CLOUDFLARE_API_TOKEN";
export const CLEF_TOKEN_ALIAS_ENV = "CLOUDFLARE_AUTH_TOKEN";
export const CLEF_MODEL_ENV = "XCB_CLEF_MODEL";
export const CLEF_IMAGE_LIMITS = { maxImages: 4, maxImageBytes: 4 * 1024 * 1024,
  maxTotalImageBytes: 8 * 1024 * 1024, maxPixels: 16_000_000, maxBodyBytes: 13 * 1024 * 1024 } as const;
const fail = (code = "JUDGE_CLEF_RESPONSE_INVALID"): never => { throw new Error(code); };

export function clefEndpoint(accountId: string, model: string = "clef"): string {
  if (!/^[a-fA-F0-9]{32}$/.test(accountId) || !CLEF_MODELS.includes(model as ClefModel)) fail("JUDGE_CLEF_TARGET_INVALID");
  return `https://api.cloudflare.com/client/v4/accounts/${accountId}/ai/run/@cf/cloudflare/${model}`;
}

const crcTable = Array.from({ length: 256 }, (_, value) => {
  let crc = value;
  for (let i = 0; i < 8; i++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0);
  return crc >>> 0;
});
function crc32(data: Buffer): number {
  let crc = 0xffffffff;
  for (const byte of data) crc = crcTable[(crc ^ byte) & 255]! ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}
function dimensions(data: Buffer, mime: string): [number, number] {
  if (mime === "image/png" && data.length >= 45 && data.subarray(0, 8).equals(Buffer.from([137,80,78,71,13,10,26,10]))) {
    let offset = 8, width = 0, height = 0;
    const compressed: Buffer[] = [];
    while (offset + 12 <= data.length) {
      const size = data.readUInt32BE(offset), type = data.toString("ascii", offset + 4, offset + 8);
      if (offset + 12 + size > data.length) break;
      if (crc32(data.subarray(offset + 4, offset + 8 + size)) !== data.readUInt32BE(offset + 8 + size)) break;
      if (offset === 8 && (type !== "IHDR" || size !== 13)) break;
      if (type === "IHDR") {
        if (width !== 0 || size !== 13) break;
        width = data.readUInt32BE(offset + 8); height = data.readUInt32BE(offset + 12);
        if (!width || !height || width * height > CLEF_IMAGE_LIMITS.maxPixels) fail("JUDGE_IMAGE_PIXELS_LIMIT");
        if (data[offset + 18] !== 0 || data[offset + 19] !== 0 || data[offset + 20]! > 1) break;
      }
      if (type === "IDAT" && size > 0) compressed.push(data.subarray(offset + 8, offset + 8 + size));
      offset += size + 12;
      if (type === "IEND" && size === 0 && offset === data.length && compressed.length > 0) {
        try {
          const decoded = inflateSync(Buffer.concat(compressed), { maxOutputLength: 128 * 1024 * 1024 });
          const depth = data[24]!, color = data[25]!, channels = ({ 0: 1, 2: 3, 3: 1, 4: 2, 6: 4 } as Record<number, number>)[color];
          if (!channels || ![1,2,4,8,16].includes(depth) || (color !== 0 && color !== 3 && depth < 8) || (color === 3 && depth === 16)) break;
          if (data[28] === 0 && decoded.length !== height * (1 + Math.ceil(width * channels * depth / 8))) break;
          return [width, height];
        } catch { break; }
      }
    }
  }
  if (mime === "image/jpeg" && data.length >= 4 && data[0] === 255 && data[1] === 216 && data[data.length - 2] === 255 && data[data.length - 1] === 217) {
    let offset = 2, width = 0, height = 0;
    while (offset + 4 <= data.length) {
      if (data[offset++] !== 255) break;
      while (data[offset] === 255) offset++;
      const marker = data[offset++]!;
      const size = data.readUInt16BE(offset);
      if (size < 2 || offset + size > data.length) break;
      if ([192,193,194,195,197,198,199,201,202,203,205,206,207].includes(marker) && size >= 8) {
        height = data.readUInt16BE(offset + 3); width = data.readUInt16BE(offset + 5);
      }
      if (marker === 218 && width > 0 && height > 0 && offset + size < data.length - 2) return [width, height];
      offset += size;
    }
  }
  if (mime === "image/webp" && data.length >= 30 && data.toString("ascii", 0, 4) === "RIFF" && data.toString("ascii", 8, 12) === "WEBP" && data.readUInt32LE(4) + 8 === data.length) {
    let offset = 12, width = 0, height = 0, pixels = false;
    while (offset + 8 <= data.length) {
      const kind = data.toString("ascii", offset, offset + 4), size = data.readUInt32LE(offset + 4), start = offset + 8;
      if (start + size > data.length) break;
      if (kind === "VP8X") {
        if (offset !== 12 || pixels || size !== 10) fail("JUDGE_IMAGE_INVALID");
        width = 1 + data.readUIntLE(start + 4, 3); height = 1 + data.readUIntLE(start + 7, 3);
      }
      if (kind === "VP8 " && size >= 10 && data.subarray(start + 3, start + 6).equals(Buffer.from([157,1,42]))) {
        const w = data.readUInt16LE(start + 6) & 16383, h = data.readUInt16LE(start + 8) & 16383;
        if (width && (width !== w || height !== h)) fail("JUDGE_IMAGE_INVALID");
        width = w; height = h; pixels = true;
      }
      if (kind === "VP8L" && size >= 5 && data[start] === 47) {
        const bits = data.readUInt32LE(start + 1);
        const w = 1 + (bits & 16383), h = 1 + ((bits >>> 14) & 16383);
        if (width && (width !== w || height !== h)) fail("JUDGE_IMAGE_INVALID");
        width = w; height = h; pixels = true;
      }
      offset = start + size + (size % 2);
    }
    if (offset === data.length && pixels) return [width, height];
  }
  return fail("JUDGE_IMAGE_INVALID");
}

export function checkClefImages(images: unknown): ClefImage[] {
  if (!Array.isArray(images) || images.length > CLEF_IMAGE_LIMITS.maxImages) fail("JUDGE_IMAGES_LIMIT");
  let total = 0;
  for (const image of images) {
    let mime: unknown, encoded: unknown;
    if (typeof image === "string") {
      if (image.length > Math.ceil(CLEF_IMAGE_LIMITS.maxImageBytes / 3) * 4 + 32) fail("JUDGE_IMAGE_LIMIT");
      const match = /^data:(image\/(?:png|jpeg|webp));base64,(.+)$/i.exec(image);
      if (!match) fail("JUDGE_IMAGE_INVALID");
      mime = match[1]!.toLowerCase(); encoded = match[2];
    } else if (image !== null && typeof image === "object" && !Array.isArray(image)) {
      if (Object.keys(image).some(key => !["content_type", "base64"].includes(key))) fail("JUDGE_IMAGE_INVALID");
      mime = image.content_type; encoded = image.base64;
    }
    if (!["image/png", "image/jpeg", "image/webp"].includes(mime as string) || typeof encoded !== "string") fail("JUDGE_IMAGE_INVALID");
    if (encoded.length > Math.ceil(CLEF_IMAGE_LIMITS.maxImageBytes / 3) * 4) fail("JUDGE_IMAGE_LIMIT");
    if (encoded.length < 4 || encoded.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(encoded)) fail("JUDGE_IMAGE_INVALID");
    const data = Buffer.from(encoded, "base64");
    if (data.toString("base64") !== encoded) fail("JUDGE_IMAGE_INVALID");
    total += data.length;
    if (data.length > CLEF_IMAGE_LIMITS.maxImageBytes || total > CLEF_IMAGE_LIMITS.maxTotalImageBytes) fail("JUDGE_IMAGE_LIMIT");
    const [width, height] = dimensions(data, mime as string);
    if (width === 0 || height === 0 || width * height > CLEF_IMAGE_LIMITS.maxPixels) fail("JUDGE_IMAGE_PIXELS_LIMIT");
  }
  return images as ClefImage[];
}

export function checkClefQuestions(questions: JudgeQuestions): void {
  checkJudgeQuestions(questions);
  for (const [id, q] of Object.entries(questions)) {
    if (!/^[A-Za-z0-9_.-]{1,100}$/.test(id) || !q.instructions.trim()) fail("JUDGE_CLEF_QUESTION_INVALID");
    if (q.type === "choice" && Object.keys(q.criteria).length < 2) fail("JUDGE_CLEF_QUESTION_INVALID");
    if (q.type === "score" && (q.criteria.length < 2 || q.criteria.length > 10)) fail("JUDGE_CLEF_QUESTION_INVALID");
  }
}

const CLEF_ROUNDING_HALF_UNIT = 0.5 * 10 ** -4;
const ROUNDING_ARITHMETIC_EPSILON = 1e-12;
type ProbabilityBounds = { lower: number; upper: number };
function roundedScoreEndpoint(bounds: ProbabilityBounds[], lowerTotal: number, maximum: boolean): number {
  let score = bounds.reduce((sum, bound, level) => sum + level * bound.lower, 0);
  let remaining = Math.max(0, 1 - lowerTotal);
  for (let position = 0; position < bounds.length && remaining > 0; position++) {
    const level = maximum ? bounds.length - 1 - position : position;
    const bound = bounds[level]!;
    const added = Math.min(remaining, bound.upper - bound.lower);
    score += level * added;
    remaining -= added;
  }
  return score;
}

export function parseClefResponse(status: number, body: string, model: string, questions: JudgeQuestions): JudgeAnswers {
  if (status < 200 || status >= 300) return parseJudgeResponse(status, "{}");
  if (Buffer.byteLength(body) > 256 * 1024) fail("JUDGE_RESPONSE_LIMIT");
  let value: unknown;
  try { value = JSON.parse(body); } catch { return fail(); }
  if (value === null || typeof value !== "object" || Array.isArray(value)) return fail();
  const envelope = value as Record<string, unknown>;
  if (envelope.success !== true || !Array.isArray(envelope.errors) || envelope.errors.length !== 0) return fail();
  const parsed = checkJudgeAnswers(questions, parseJudgeResponse(status, JSON.stringify(envelope.result)));
  if (parsed.model !== model || parsed.usage?.input_tokens === undefined || parsed.usage.output_tokens === undefined) return fail();
  for (const [id, q] of Object.entries(questions)) {
    const answer = parsed.answers[id]!;
    if (answer.type !== q.type) return fail();
    const raw = (envelope.result as { answers: Record<string, Record<string, unknown>> }).answers[id]!;
    if (raw.type !== q.type) return fail();
    if (answer.type === "noul") continue;
    const expected = q.type === "choice" ? Object.keys(q.criteria) : (q as { criteria: string[] }).criteria.map((_, i) => String(i));
    if (Object.keys(answer.probabilities).length !== expected.length || expected.some(key => !Object.hasOwn(answer.probabilities, key))) return fail();
    const bounds = expected.map(key => ({
      lower: Math.max(0, answer.probabilities[key]! - CLEF_ROUNDING_HALF_UNIT),
      upper: Math.min(1, answer.probabilities[key]! + CLEF_ROUNDING_HALF_UNIT),
    }));
    const lowerTotal = bounds.reduce((sum, bound) => sum + bound.lower, 0);
    const upperTotal = bounds.reduce((sum, bound) => sum + bound.upper, 0);
    if (lowerTotal > 1 + ROUNDING_ARITHMETIC_EPSILON || upperTotal < 1 - ROUNDING_ARITHMETIC_EPSILON) return fail();
    if (answer.type === "choice" && answer.probabilities[answer.choice]! < Math.max(...Object.values(answer.probabilities))) return fail();
    if (answer.type === "score" && q.type === "score") {
      const legend = raw.legend;
      if (legend === null || typeof legend !== "object" || Array.isArray(legend) || Object.keys(legend).length !== expected.length || expected.some((key, i) => (legend as Record<string, unknown>)[key] !== q.criteria[i])) return fail();
      const minimum = roundedScoreEndpoint(bounds, lowerTotal, false);
      const maximum = roundedScoreEndpoint(bounds, lowerTotal, true);
      if (answer.score + CLEF_ROUNDING_HALF_UNIT < minimum - ROUNDING_ARITHMETIC_EPSILON ||
        answer.score - CLEF_ROUNDING_HALF_UNIT > maximum + ROUNDING_ARITHMETIC_EPSILON) return fail();
      answer.legend = legend as Record<string, string>;
    }
  }
  return parsed;
}

export interface ClefOptions {
  accountId: string;
  token: string;
  model?: string;
  endpoint?: string;
  fetch?: typeof fetch;
  timeoutMs?: number;
}
export function createClefJudge(options: ClefOptions): Judge {
  const model = options.model ?? "clef", endpoint = clefEndpoint(options.accountId, model);
  if (options.endpoint !== undefined && options.endpoint !== endpoint) fail("JUDGE_CLEF_TARGET_INVALID");
  if (!/^[A-Za-z0-9_\-.=+]{1,512}$/.test(options.token)) fail("JUDGE_KEY_INVALID");
  const timeout = options.timeoutMs ?? 15000;
  if (!Number.isInteger(timeout) || timeout < 1000 || timeout > 120000) fail("JUDGE_TIMEOUT_INVALID");
  const fetcher = options.fetch ?? fetch;
  return Object.freeze({ async ask(state: JudgeState, questions: JudgeQuestions, input: JudgeAskOptions = {}) {
    checkJudgeState(state); checkClefQuestions(questions);
    input.signal?.throwIfAborted();
    const images = input.images === undefined ? undefined : checkClefImages(input.images);
    const body = JSON.stringify({ model, state, questions, ...(images === undefined ? {} : { images }) });
    if (Buffer.byteLength(body) > CLEF_IMAGE_LIMITS.maxBodyBytes) fail("JUDGE_REQUEST_LIMIT");
    const signal = input.signal ? AbortSignal.any([input.signal, AbortSignal.timeout(timeout)]) : AbortSignal.timeout(timeout);
    const response = await fetcher(endpoint, { method: "POST", redirect: "error", signal,
      headers: { authorization: `Bearer ${options.token}`, "content-type": "application/json", accept: "application/json" }, body });
    const reader = response.body?.getReader();
    const chunks: Uint8Array[] = []; let size = 0;
    try {
      if (reader) for (;;) {
        const { done, value } = await reader.read(); if (done) break;
        size += value.byteLength; if (size > 256 * 1024) { await reader.cancel().catch(() => {}); fail("JUDGE_RESPONSE_LIMIT"); }
        chunks.push(value);
      }
    } finally { reader?.releaseLock(); }
    const text = reader ? Buffer.concat(chunks).toString("utf8") : await response.text();
    return parseClefResponse(response.status, text, model, questions);
  } });
}
