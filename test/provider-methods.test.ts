import { expect, test } from "bun:test";

const inventory = await Bun.file(new URL("../crates/xcb-runtime/src/provider-methods.json", import.meta.url)).json();

test("Claude method accounting matches every Query method in the pinned SDK", async () => {
  const declarations = await Bun.file(new URL("../node_modules/@anthropic-ai/claude-agent-sdk/sdk.d.ts", import.meta.url)).text();
  const query = declarations.split("export declare interface Query extends ")[1]?.split("\n}")[0];
  if (!query) throw new Error("Claude Query interface missing");
  const signatures = query.replace(/\/\*[\s\S]*?\*\//gu, "");
  const methods = [...signatures.matchAll(/^ {4}(\w+)\(/gmu)].map(match => match[1]!).sort();
  expect(methods).toEqual(inventory.claude.Query);
  expect(new Set(methods).size).toBe(methods.length);
  const sdk = await Bun.file(new URL("../node_modules/@anthropic-ai/claude-agent-sdk/package.json", import.meta.url)).json();
  expect(sdk.version).toBe(inventory.claude.sdkVersion);
});

test("Codex method inventory binds the checked schema and keeps each direction unique", () => {
  expect(inventory.codex.schemaSha256).toMatch(/^[a-f0-9]{64}$/u);
  for (const methods of Object.values(inventory.codex.methods) as string[][]) {
    expect(new Set(methods).size).toBe(methods.length);
    expect(methods).toEqual([...methods].sort());
  }
  expect(inventory.codex.methods.ServerRequest).toContain("item/tool/call");
  expect(inventory.codex.methods.ClientRequest).toContain("turn/interrupt");
});
