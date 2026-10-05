import { expect, test } from "bun:test";
import { parseClefResponse } from "../src/clef.ts";
import { checkClefImages, createClefJudge, createSystemOneJudge, clefEndpoint, resolveJudge, type JudgeQuestions } from "../src/judge.ts";

const accountId = "a".repeat(32);
const questions: JudgeQuestions = {
  pick: { type: "choice", instructions: "Which?", criteria: { a: "first", b: "second" } },
  score: { type: "score", instructions: "Rate", criteria: ["low", "high"] },
};
const result = () => ({ model: "clef", usage: { input_tokens: 10, output_tokens: 2 }, answers: {
  pick: { type: "choice", choice: "b", confidence: 0.8, probabilities: { a: 0.2, b: 0.8 } },
  score: { type: "score", score: 0.8, confidence: 0.8, probabilities: { "0": 0.2, "1": 0.8 }, legend: { "0": "low", "1": "high" } },
} });
const recordedQuestions: JudgeQuestions = {
  urgent: { type: "noul", instructions: "Is the outage urgent?" },
  team: { type: "choice", instructions: "Which team should handle the outage?", criteria: { technical: "Outages and errors", sales: "Sales inquiries" } },
  severity: { type: "score", instructions: "How severe is the customer impact?", criteria: ["No impact", "Minor", "Major", "Critical"] },
};
const recordedResults = [
  { model: "clef", usage: { input_tokens: 319, output_tokens: 0 }, answers: {
    urgent: { type: "noul", noul: 0.9869 },
    team: { type: "choice", choice: "technical", probabilities: { technical: 0.9635, sales: 0.0365 }, confidence: 0.8593 },
    severity: { type: "score", score: 2.931, legend: { "0": "No impact", "1": "Minor", "2": "Major", "3": "Critical" },
      probabilities: { "0": 0.0047, "1": 0.0051, "2": 0.0448, "3": 0.9454 }, confidence: 0.8612 },
  } },
  { model: "clef-flash", usage: { input_tokens: 319, output_tokens: 0 }, answers: {
    urgent: { type: "noul", noul: 0.9354 },
    team: { type: "choice", choice: "technical", probabilities: { technical: 0.9724, sales: 0.0276 }, confidence: 0.8928 },
    severity: { type: "score", score: 2.7378, legend: { "0": "No impact", "1": "Minor", "2": "Major", "3": "Critical" },
      probabilities: { "0": 0.0149, "1": 0.0157, "2": 0.186, "3": 0.7834 }, confidence: 0.5316 },
  } },
];
for (const recorded of recordedResults) {
  test(`Clef accepts recorded 2026-10-04 synthetic ${recorded.model} provider rounding`, () => {
    const parsed = parseClefResponse(200, JSON.stringify({ success: true, errors: [], result: recorded }), recorded.model, recordedQuestions);
    expect(parsed).toEqual(recorded);
  });
}
const png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

function crc(data: Buffer): number {
  let value = 0xffffffff;
  for (const byte of data) { value ^= byte; for (let i = 0; i < 8; i++) value = (value >>> 1) ^ ((value & 1) ? 0xedb88320 : 0); }
  return (value ^ 0xffffffff) >>> 0;
}
function imageWithPadding(size: number): string {
  const data = Buffer.from(png.split(",")[1]!, "base64"), chunk = Buffer.alloc(size + 12);
  chunk.writeUInt32BE(size); chunk.write("raNd", 4);
  chunk.writeUInt32BE(crc(chunk.subarray(4, size + 8)), size + 8);
  return `data:image/png;base64,${Buffer.concat([data.subarray(0, 33), chunk, data.subarray(33)]).toString("base64")}`;
}
function client(value: unknown, status = 200) {
  const calls: RequestInit[] = [];
  const judge = createClefJudge({ accountId, token: "synthetic-token", fetch: (async (_url, init) => {
    calls.push(init!);
    return new Response(JSON.stringify(value), { status });
  }) as typeof fetch });
  return { judge, calls };
}

test("Clef uses only the fixed account/model endpoint and wrapped REST results", async () => {
  expect(clefEndpoint(accountId, "clef-flash")).toBe(`https://api.cloudflare.com/client/v4/accounts/${accountId}/ai/run/@cf/cloudflare/clef-flash`);
  expect(() => clefEndpoint("not-an-account", "clef")).toThrow();
  expect(() => createClefJudge({ accountId, token: "synthetic", endpoint: "https://example.com/ask" })).toThrow();
  const { judge, calls } = client({ success: true, errors: [], result: result() });
  const answers = await judge.ask("evidence", questions, { images: [png] });
  expect(answers.model).toBe("clef");
  expect(answers.answers.score?.type === "score" && answers.answers.score.legend).toEqual({ "0": "low", "1": "high" });
  expect(JSON.parse(String(calls[0]!.body)).images).toEqual([png]);
  expect(calls[0]!.redirect).toBe("error");
  expect(new Headers(calls[0]!.headers).get("authorization")).toBe("Bearer synthetic-token");
});

test("Clef rejects bare/error envelopes, wrong model, incomplete options and altered score legends", async () => {
  const bad = [result(), { success: false, result: result(), errors: [] },
    { success: true, result: result(), errors: [{ code: 1, message: "secret upstream text" }] }];
  for (const mutation of [
    (r: ReturnType<typeof result>) => { r.model = "clef-flash"; },
    (r: ReturnType<typeof result>) => { delete (r.answers.pick.probabilities as Record<string, number>).a; },
    (r: ReturnType<typeof result>) => { r.answers.score.legend["0"] = "changed"; },
    (r: ReturnType<typeof result>) => { r.answers.score.score = 1; },
    (r: ReturnType<typeof result>) => { r.answers.pick.choice = "a"; },
  ]) { const r = result(); mutation(r); bad.push({ success: true, errors: [], result: r }); }
  for (const value of bad) {
    const { judge, calls } = client(value);
    await expect(judge.ask("evidence", questions)).rejects.toThrow();
    expect(calls).toHaveLength(1);
  }
  const { judge, calls } = client({}, 429);
  await expect(judge.ask("evidence", questions)).rejects.toThrow("JUDGE_RATE_LIMITED");
  expect(calls).toHaveLength(1);
});

test("Clef rounding requires a normalized distribution and an attainable score", () => {
  const parse = (value: ReturnType<typeof result>) => parseClefResponse(200,
    JSON.stringify({ success: true, errors: [], result: value }), "clef", questions);
  for (const [zero, one, score] of [[0.2, 0.7999, 0.8], [0.2, 0.8001, 0.8], [0, 1, 0.9999]]) {
    const value = result();
    value.answers.pick.probabilities = { a: zero!, b: one! };
    value.answers.score.probabilities = { "0": zero!, "1": one! };
    value.answers.score.score = score!;
    expect(() => parse(value)).not.toThrow();
  }
  for (const [zero, one, score] of [
    [0.2001, 0.8001, 0.8], [0.1999, 0.7999, 0.8], [0.2, 0.8, 0.80011],
    [0.2, 0.8001, 0.7999], [0, 1, 0.99989], [0, 0.99989, 0.9999], [0, 0, 0],
  ]) {
    const value = result();
    value.answers.score.probabilities = { "0": zero!, "1": one! };
    value.answers.score.score = score!;
    expect(() => parse(value)).toThrow();
  }
  for (const probabilities of [{ a: 0.2001, b: 0.8001 }, { a: 0.1999, b: 0.7999 }, { a: 0, b: 0.99989 }]) {
    const value = result();
    value.answers.pick.probabilities = probabilities;
    expect(() => parse(value)).toThrow();
  }
});

test("Clef accepts rounded normalized distributions across score levels", () => {
  const round = (value: number) => Math.round(value * 10_000) / 10_000;
  for (let levels = 2; levels <= 10; levels++) for (let seed = 0; seed < 16; seed++) {
    const weights = Array.from({ length: levels }, (_, level) => (seed * 7 + level * 11) % 17);
    const total = weights.reduce((sum, weight) => sum + weight, 0);
    const original = weights.map(weight => weight / total);
    const criteria = weights.map((_, level) => `level ${level}`);
    const legend = Object.fromEntries(criteria.map((criterion, level) => [String(level), criterion]));
    const probabilities = Object.fromEntries(original.map((probability, level) => [String(level), round(probability)]));
    const score = round(original.reduce((sum, probability, level) => sum + level * probability, 0));
    const answer = { type: "score", score, probabilities, legend, confidence: 0.5 };
    const value = { model: "clef", usage: { input_tokens: 1, output_tokens: 0 }, answers: { score: answer } };
    expect(parseClefResponse(200, JSON.stringify({ success: true, errors: [], result: value }), "clef", {
      score: { type: "score", instructions: "Rate", criteria },
    }).answers.score).toEqual(answer);
  }
});

test("image and cancellation failures happen before any paid call", async () => {
  const { judge, calls } = client({ success: true, errors: [], result: result() });
  for (const images of [["https://example.com/a.png"], ["data:image/png;base64,AAAA"], Array(5).fill(png),
    [{ content_type: "image/png", base64: "a".repeat(6 * 1024 * 1024) }]]) {
    await expect(judge.ask("evidence", questions, { images })).rejects.toThrow();
  }
  const padded = imageWithPadding(3 * 1024 * 1024);
  await expect(judge.ask("evidence", questions, { images: [padded, padded, padded] })).rejects.toThrow("JUDGE_IMAGE_LIMIT");
  await expect(judge.ask("evidence", questions, { images: [imageWithPadding(4 * 1024 * 1024)] })).rejects.toThrow("JUDGE_IMAGE_LIMIT");
  const pixels = Buffer.from(png.split(",")[1]!, "base64");
  pixels.writeUInt32BE(4001, 16); pixels.writeUInt32BE(4000, 20); pixels.writeUInt32BE(crc(pixels.subarray(12, 29)), 29);
  await expect(judge.ask("evidence", questions, { images: [`data:image/png;base64,${pixels.toString("base64")}`] })).rejects.toThrow("JUDGE_IMAGE_PIXELS_LIMIT");
  const abort = new AbortController(); abort.abort();
  await expect(judge.ask("evidence", questions, { signal: abort.signal })).rejects.toThrow();
  expect(calls).toHaveLength(0);
});

test("response streaming is bounded and failures are not retried", async () => {
  let calls = 0, cancelled = false;
  const judge = createClefJudge({ accountId, token: "synthetic", fetch: (async () => {
    calls++;
    return new Response(new ReadableStream({ start(controller) { controller.enqueue(new Uint8Array(256 * 1024 + 1)); }, cancel() { cancelled = true; } }));
  }) as typeof fetch });
  await expect(judge.ask("evidence", questions)).rejects.toThrow("JUDGE_RESPONSE_LIMIT");
  expect(calls).toBe(1); expect(cancelled).toBe(true);
});

test("Clef request body is capped and legacy adapters reject supplied images", async () => {
  const { judge, calls } = client({ success: true, errors: [], result: result() });
  const large = Object.fromEntries(Array.from({ length: 64 }, (_, i) => [`q${i}`, {
    type: "choice" as const, instructions: "Choose", criteria: Object.fromEntries(Array.from({ length: 64 }, (_, j) => [`o${j}`, "x".repeat(4096)])),
  }]));
  await expect(judge.ask("evidence", large)).rejects.toThrow("JUDGE_REQUEST_LIMIT");
  expect(calls).toHaveLength(0);
  const legacy = createSystemOneJudge({ token: "synthetic", fetch: (async () => { throw new Error("must not call"); }) as typeof fetch });
  await expect(legacy.ask("evidence", questions, { images: [png] })).rejects.toThrow("JUDGE_IMAGES_UNSUPPORTED");
  expect(() => createSystemOneJudge({ token: "legacy", endpoint: clefEndpoint(accountId) })).toThrow("JUDGE_PROVIDER_ENDPOINT_MISMATCH");
  expect(checkClefImages([{ content_type: "image/png", base64: png.split(",")[1] }])).toHaveLength(1);
});

test("default resolution requires Cloudflare env credentials and never reuses TypeSafe keys", async () => {
  const env = (values: Record<string, string>) => (name: string) => values[name];
  expect(await resolveJudge({ stateRoot: "/unused", enabled: true, env: env({ XCB_JEV_API_KEY: "legacy-token" }) })).toBeNull();
  expect(await resolveJudge({ stateRoot: "/unused", enabled: true, env: env({ CLOUDFLARE_ACCOUNT_ID: accountId, CLOUDFLARE_API_TOKEN: "synthetic" }) })).not.toBeNull();
  await expect(resolveJudge({ stateRoot: "/unused", enabled: true, env: env({ CLOUDFLARE_ACCOUNT_ID: "invalid", CLOUDFLARE_API_TOKEN: "synthetic" }) })).rejects.toThrow();
  expect(await resolveJudge({ stateRoot: "/unused", enabled: false, env: env({ CLOUDFLARE_ACCOUNT_ID: "invalid", CLOUDFLARE_API_TOKEN: "invalid token" }) })).toBeNull();
  expect(await resolveJudge({ stateRoot: "/unused", enabled: true, env: env({ CLOUDFLARE_ACCOUNT_ID: accountId, CLOUDFLARE_AUTH_TOKEN: "synthetic-alias" }) })).not.toBeNull();
  await expect(resolveJudge({ stateRoot: "/unused", enabled: true, env: env({ CLOUDFLARE_ACCOUNT_ID: accountId, CLOUDFLARE_API_TOKEN: "invalid token", CLOUDFLARE_AUTH_TOKEN: "valid-alias" }) })).rejects.toThrow();
});
