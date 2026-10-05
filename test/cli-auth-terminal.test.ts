import { describe, expect, test } from "bun:test";
import { spawn, spawnSync } from "node:child_process";
import { mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { waitForAuthChild, withAuthTerminal } from "../src/cli/auth-terminal.ts";

const python = "/usr/bin/python3";
const hasPty = process.platform !== "win32"
  && spawnSync(python, ["-c", "import pty, termios"], { stdio: "ignore", timeout: 2_000 }).status === 0;
const token = "sk-ant-oat01-synthetic_terminal_regression_not_for_service_use";
const shellQuote = (text: string) => "'" + text.replaceAll("'", "'\\''") + "'";
const stty = process.platform === "linux" ? "/usr/bin/stty" : "/bin/stty";

type Receipt = { before: string; after: string; error: string | null; token: string | null;
  handlersBefore: number[]; handlersAfter: number[]; subsequentSignal: number;
  flowingBefore: boolean | null; flowingAfter: boolean | null };

async function ptyCase(provider: "claude", mode: "success" | "failure" | "cancel" | "cancel-status" | "retry" | "missing" | "timeout") {
  const root = await realpath(await mkdtemp(join(tmpdir(), "xcb-auth-pty-")));
  const executable = join(root, "provider"), calls = join(root, "calls"), normalized = join(root, "normalized");
  const receipt = join(root, "receipt.json"), runner = join(root, "runner.ts");
  const inspection = { provider, executablePath: executable, version: "2.1.268",
    sha256: "0".repeat(64), pinnedSha256: null, versionMatches: true, digestMatches: true };
  const stub = `#!/bin/sh
printf '%s\\n' "$*" >> ${shellQuote(calls)}
${stty} -a >> ${shellQuote(normalized)}
printf 'NEED_CODE\\n'
IFS= read -r code
[ "$code" = synthetic-code ] || exit 91
${stty} raw -echo
${mode === "failure" ? "exit 2" : mode === "cancel-status" ? "exit 130" : mode === "timeout" ? "while :; do :; done" : mode === "retry" ? `[ "$(wc -l < ${shellQuote(calls)})" -ne 1 ] || exit 2` : ""}
${provider === "claude" ? `printf '%s\\n' ${shellQuote(token)}` : "printf 'signed in\\n'"}
exit 0
`;
  const auth = new URL("../src/cli/auth.ts", import.meta.url).href;
  const terminal = new URL("../src/cli/auth-terminal.ts", import.meta.url).href;
  const script = `import { spawn, spawnSync } from "node:child_process";
import { writeFile } from "node:fs/promises";
import { claudeLogin, readClaudeOAuthToken } from ${JSON.stringify(auth)};
import { waitForAuthChild, withAuthTerminal } from ${JSON.stringify(terminal)};
const state = () => spawnSync(${JSON.stringify(stty)}, ["-g"], { stdio: [0, "pipe", "pipe"], encoding: "utf8" }).stdout.trim();
const signals = ["SIGINT", "SIGTERM", "SIGHUP"];
const handlersBefore = signals.map(signal => process.listenerCount(signal));
const flowingBefore = process.stdin.readableFlowing;
const before = state(); let error = null;
try { ${mode === "timeout" ? `await withAuthTerminal(async () => {
  const child = spawn(${JSON.stringify(executable)}, [], { stdio: "inherit", env: { PATH: "/usr/bin:/bin" } });
  if ((await waitForAuthChild(child, 500)).timedOut) throw new Error("SYNTHETIC_AUTH_TIMEOUT");
});` : `await claudeLogin(${JSON.stringify(root)}, ${JSON.stringify(inspection)});`} }
catch (caught) { error = caught instanceof Error ? caught.message : String(caught); }
const handlersAfter = signals.map(signal => process.listenerCount(signal));
let subsequentSignal = 0;
process.once("SIGINT", () => { subsequentSignal++; });
process.emit("SIGINT");
await writeFile(${JSON.stringify(receipt)}, JSON.stringify({ before, after: state(), error, handlersBefore, handlersAfter, subsequentSignal,
  flowingBefore, flowingAfter: process.stdin.readableFlowing,
  token: await readClaudeOAuthToken(${JSON.stringify(root)}) }));
`;
  try {
    if (mode !== "missing") await writeFile(executable, stub, { mode: 0o700 });
    await writeFile(runner, script);
    const child = Bun.spawn([python, fileURLToPath(new URL("./fixtures/auth-terminal-pty.py", import.meta.url)),
      process.execPath, runner, mode], { stdin: "ignore", stdout: "pipe", stderr: "pipe", timeout: 15_000 });
    const [code, stdout, stderr] = await Promise.all([child.exited, new Response(child.stdout).text(), new Response(child.stderr).text()]);
    expect(stderr).toBe("");
    expect(code).toBe(0);
    const driver = JSON.parse(stdout) as { exit: number; sent: number; output: string };
    expect(driver.exit).toBe(0);
    const result = JSON.parse(await readFile(receipt, "utf8")) as Receipt;
    expect(result.after).toBe(result.before);
    expect(result.handlersAfter).toEqual(result.handlersBefore);
    expect(result.subsequentSignal).toBe(1);
    expect(result.flowingAfter).toBe(result.flowingBefore);
    const attempts = await readFile(calls, "utf8").catch(() => "");
    const flags = await readFile(normalized, "utf8").catch(() => "");
    if (mode !== "missing") {
      expect(flags).toMatch(/(?:^|[ ;\n])icanon(?:[ ;\n]|$)/u);
      expect(flags).toMatch(/(?:^|[ ;\n])icrnl(?:[ ;\n]|$)/u);
      expect(flags).not.toMatch(/(?:^|[ ;\n])-icanon(?:[ ;\n]|$)/u);
      expect(flags).not.toMatch(/(?:^|[ ;\n])-icrnl(?:[ ;\n]|$)/u);
    }
    return { result, driver, attempts };
  } finally { await rm(root, { recursive: true, force: true }); }
}

describe("compatibility provider terminal handoff", () => {
  test.skipIf(!hasPty)("Claude accepts Enter from inherited raw mode and masks its token", async () => {
    const { result, driver, attempts } = await ptyCase("claude", "success");
    expect(result.error).toBeNull();
    expect(result.token).toBe(token);
    expect(driver.output).not.toContain(token);
    expect(attempts.trim()).toBe("setup-token");
  }, 20_000);

  test.skipIf(!hasPty)("failed and missing providers restore the exact state", async () => {
    expect((await ptyCase("claude", "failure")).result.error).toBe("CLAUDE_LOGIN_FAILED");
    expect((await ptyCase("claude", "missing")).result.error).toBe("CLAUDE_LOGIN_FAILED");
  }, 35_000);

  test.skipIf(!hasPty)("Claude normalizes every fallback attempt and restores the original state", async () => {
    const { result, driver, attempts } = await ptyCase("claude", "retry");
    expect(result.error).toBeNull();
    expect(result.token).toBe(token);
    expect(attempts.trim().split("\n")).toEqual(["setup-token", "auth login", "setup-token"]);
    expect(driver.sent).toBe(3);
    expect(driver.output).not.toContain(token);
  }, 20_000);

  test.skipIf(!hasPty)("Ctrl+C restores the terminal and never triggers a Claude fallback", async () => {
    const { result, attempts } = await ptyCase("claude", "cancel");
    expect(result.error).toBe("CLAUDE_LOGIN_CANCELLED");
    expect(result.token).toBeNull();
    expect(attempts.trim()).toBe("setup-token");
  }, 20_000);

  test.skipIf(!hasPty)("a provider-handled cancellation also prevents fallback", async () => {
    const { result, attempts } = await ptyCase("claude", "cancel-status");
    expect(result.error).toBe("CLAUDE_LOGIN_CANCELLED");
    expect(result.token).toBeNull();
    expect(attempts.trim()).toBe("setup-token");
  }, 20_000);

  test.skipIf(!hasPty)("timeout joins the owned child and restores the exact terminal state", async () => {
    const { result } = await ptyCase("claude", "timeout");
    expect(result.error).toBe("SYNTHETIC_AUTH_TIMEOUT");
  }, 20_000);

  test.skipIf(process.platform === "win32")("non-TTY children are bounded and joined before the terminal guard returns", async () => {
    const child = spawn("/bin/sh", ["-c", "while :; do :; done"], { stdio: "ignore" });
    let closed = false;
    child.once("close", () => { closed = true; });
    const result = await withAuthTerminal(() => waitForAuthChild(child, 60));
    expect(result.timedOut).toBe(true);
    expect(result.code).toBe(1);
    expect(closed).toBe(true);
    expect(child.signalCode).toBe("SIGKILL");
  });

  test("spawn errors settle only after close and remove temporary signal handlers", async () => {
    const before = ["SIGINT", "SIGTERM", "SIGHUP"].map(signal => process.listenerCount(signal));
    const child = spawn("/not/an/xcb-auth-fixture", [], { stdio: "ignore" });
    let closed = false;
    child.once("close", () => { closed = true; });
    expect((await waitForAuthChild(child, 1_000)).code).toBe(1);
    expect(closed).toBe(true);
    expect(["SIGINT", "SIGTERM", "SIGHUP"].map(signal => process.listenerCount(signal))).toEqual(before);
  });
});
