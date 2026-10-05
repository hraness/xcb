import { CliSessionStore } from "../src/cli/sessions.ts";
import { openAccountDatabase } from "../src/sqlite-port.ts";
import { describe, expect, test } from "bun:test";
import { mkdtemp, realpath, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const ROOT = resolve(import.meta.dir, "..");
const CLI = join(ROOT, "src", "cli.ts");

async function stateDir(): Promise<string> {
  return await realpath(await mkdtemp(join(tmpdir(), "xcb-cli-test-")));
}

/** Run the CLI source under Bun in an isolated state root with provider
 * discovery pinned to paths that cannot exist. */
async function cli(args: readonly string[], input?: string, state?: string, env: Record<string, string> = {}): Promise<{ code: number; stdout: string; stderr: string }> {
  const root = state ?? await stateDir();
  const child = Bun.spawn([process.execPath, CLI, ...args], {
    cwd: ROOT,
    env: {
      ...process.env, XCB_STATE: root, NO_COLOR: "1",
      XCB_CLAUDE: join(root, "no-such-claude"), XCB_CODEX: join(root, "no-such-codex"),
      PATH: join(root, "empty-path"), HOME: root,
      // Bun's transpiler cache otherwise creates Library/Caches in the fake
      // HOME, independently of CLI application or updater state.
      BUN_RUNTIME_TRANSPILER_CACHE_PATH: "0",
      CLOUDFLARE_ACCOUNT_ID: "", CLOUDFLARE_API_TOKEN: "", CLOUDFLARE_AUTH_TOKEN: "", XCB_JUDGE_PROVIDER: "clef", XCB_CLEF_MODEL: "clef",
      ...env,
    },
    stdin: input === undefined ? "ignore" : "pipe",
    stdout: "pipe", stderr: "pipe",
  });
  if (input !== undefined) { child.stdin!.write(input); child.stdin!.end(); }
  const [code, stdout, stderr] = await Promise.all([child.exited, new Response(child.stdout).text(), new Response(child.stderr).text()]);
  return { code, stdout, stderr };
}

describe("xcb CLI", () => {
  test("--version prints the package version", async () => {
    const { code, stdout } = await cli(["--version"]);
    expect(code).toBe(0);
    expect(stdout.trim()).toMatch(/^\d+\.\d+\.\d+$/u);
  });

  test("--help prints the command surface", async () => {
    const { code, stdout } = await cli(["--help"]);
    expect(code).toBe(0);
    for (const command of ["auth claude", "auth codex", "auth status", "auth logout", "doctor", "sessions", "resume", "run [-p", "--cwd", "update check", "update status", "update disable"]) expect(stdout).toContain(command);
  });

  test("update commands run before opening application state and refuse source updates", async () => {
    const root = join(await stateDir(), "not-created");
    for (const operation of ["status", "check", "disable"]) {
      const result = await cli(["update", operation, "--json"], undefined, root);
      expect(result.code).toBe(operation === "status" ? 0 : 1);
      const report = JSON.parse(result.stdout) as Record<string, unknown>;
      expect(report.package).toBe("@hraness/xcb");
      expect(report.status).toBe("unsupported");
      expect(report.supported).toBe(false);
      expect(await stat(root).catch(() => null)).toBeNull();
    }
  });

  test("help and version keep both application and updater state untouched", async () => {
    const root = join(await stateDir(), "not-created");
    for (const command of ["--help", "--version", "help", "-v"]) {
      expect((await cli([command, "ignored"], undefined, root)).code).toBe(0);
      expect(await stat(root).catch(() => null)).toBeNull();
    }
  });

  test("doctor reports missing providers and exits nonzero", async () => {
    const { code, stdout } = await cli(["doctor"]);
    expect(code).toBe(1);
    expect(stdout).toContain("claude: not found");
    expect(stdout).toContain("codex: not found");
  });

  test("Clef help, status and test use separate environment auth without inference or token output", async () => {
    const env = { CLOUDFLARE_ACCOUNT_ID: "a".repeat(32), CLOUDFLARE_API_TOKEN: "synthetic-clef-cli-token" };
    const help = await cli(["--help"]);
    expect(help.stdout).toContain("CLOUDFLARE_ACCOUNT_ID");
    for (const command of [["judge", "status"], ["judge", "test"]]) {
      const result = await cli(command, undefined, undefined, env);
      expect(result.code, result.stderr).toBe(0);
      expect(result.stdout).toContain("configuration valid");
      expect(result.stdout + result.stderr).not.toContain(env.CLOUDFLARE_API_TOKEN);
    }
    const rejected = await cli(["judge", "token"], "synthetic-legacy-token");
    expect(rejected.code).toBe(2);
    expect(rejected.stderr).toContain("environment only");
  });

  test("auth claude refuses when the pinned binary is absent", async () => {
    const { code, stderr } = await cli(["auth", "claude"]);
    expect(code).toBe(2);
    expect(stderr).toContain("claude binary not found");
  });

  test("auth codex refuses when the pinned binary is absent", async () => {
    const { code, stderr } = await cli(["auth", "codex"]);
    expect(code).toBe(2);
    expect(stderr).toContain("codex binary not found");
  });

  test("run refuses before provider admission", async () => {
    const { code, stderr } = await cli(["run", "-p", "hello"]);
    expect(code).toBe(2);
    expect(stderr).toContain("provider not admitted");
  });

  test("run and chat refuse a workspace containing private state before provider admission", async () => {
    const state = await stateDir();
    for (const args of [["run", "-p", "hi", "--cwd", state], ["chat", state]]) {
      const result = await cli(args, undefined, state);
      expect(result.code).not.toBe(0);
      expect(result.stderr).toContain("overlaps private xcb state");
    }
  });

  test("run rejects an unknown provider", async () => {
    const { code, stderr } = await cli(["run", "--provider", "gemini", "-p", "hi"]);
    expect(code).toBe(2);
    expect(stderr).toContain("unknown provider");
  });

  test("chat refuses before provider admission instead of hanging", async () => {
    const { code, stderr } = await cli([], "hello\n");
    expect(code).toBe(2);
    expect(stderr).toContain("claude binary not found");
  });

  test("chat reports a missing workspace path", async () => {
    const { code, stderr } = await cli(["/definitely/not/a/real/path-xyz"]);
    expect(code).toBe(2);
    expect(stderr).toContain("does not exist");
  });

  test("resume refuses an unknown session id", async () => {
    const { code, stderr } = await cli(["resume", "s_nonexistent"]);
    expect(code).toBe(2);
    expect(stderr).toContain("session not found");
  });

  test("lists retired-provider sessions but refuses resume without creating provider state", async () => {
    const root = await stateDir();
    const sessions = await CliSessionStore.open(join(root, "sessions"));
    const session = await sessions.create({ provider: "claude", accountId: "local", workspace: ROOT, model: "retired-model", now: 1 });
    sessions.close();
    const database = await openAccountDatabase(join(root, "sessions", "sessions.sqlite"));
    database.query("UPDATE xcb_cli_sessions SET provider='devin' WHERE id=?").run(session.id);
    database.close();
    const listed = await cli(["sessions"], "", root);
    expect(listed.code, listed.stderr).toBe(0);
    expect(listed.stdout).toContain(session.id);
    expect(listed.stdout).toContain("devin");
    for (const args of [["resume", session.id], ["resume", session.id, "--provider", "claude"]]) {
      const resumed = await cli(args, "", root);
      expect(resumed.code, resumed.stderr).toBe(2);
      expect(resumed.stderr).toContain("Devin support was removed");
    }
    await expect(stat(join(root, "account-leases.sqlite"))).rejects.toMatchObject({ code: "ENOENT" });
    const reopened = await CliSessionStore.open(join(root, "sessions"));
    try { expect(reopened.get(session.id)?.provider).toBe("devin"); }
    finally { reopened.close(); }
  });

  test("resume inherits the stored provider when --provider is omitted", async () => {
    const root = await stateDir();
    const sessions = await CliSessionStore.open(join(root, "sessions"));
    const session = await sessions.create({ provider: "codex", accountId: "local", workspace: ROOT, model: "gpt-5.1-codex-mini", now: Date.now() });
    sessions.close();
    const resumed = await cli(["resume", session.id], "", root);
    expect(resumed.code, resumed.stderr).toBe(2);
    expect(resumed.stderr).toContain("codex binary not found");
    expect(resumed.stderr).not.toContain("claude binary not found");
    const mismatched = await cli(["resume", session.id, "--provider", "claude"], "", root);
    expect(mismatched.code).toBe(2);
    expect(mismatched.stderr).toContain("belongs to provider codex");
  });

  test("resume without an id reports no sessions on a fresh state root", async () => {
    const { code, stderr } = await cli(["resume"]);
    expect(code).toBe(2);
    expect(stderr).toContain("no sessions yet");
  });

  test("auth status reports signed out on a fresh state root", async () => {
    const { code, stdout } = await cli(["auth", "status"]);
    expect(code).toBe(1);
    expect(stdout).toContain("signed out");
  });

  test("auth logout clears the stored token and status flips", async () => {
    const state = await stateDir();
    const { writeFile } = await import("node:fs/promises");
    await writeFile(join(state, "claude-oauth-token"), `sk-ant-oat01-${"x".repeat(64)}\n`, { mode: 0o600 });
    const before = await cli(["auth", "status"], undefined, state);
    expect(before.code).toBe(0);
    expect(before.stdout).toContain("signed in");
    const out = await cli(["auth", "logout"], undefined, state);
    expect(out.code).toBe(0);
    const after = await cli(["auth", "status"], undefined, state);
    expect(after.code).toBe(1);
    expect(after.stdout).toContain("signed out");
  });

  test("run --cwd rejects a missing workspace path", async () => {
    const { code, stderr } = await cli(["run", "-p", "hi", "--cwd", "/definitely/not/real-xyz"]);
    expect(code).toBe(2);
    expect(stderr).toContain("does not exist");
  });

  test("auth with extra arguments is a usage error", async () => {
    const { code, stderr } = await cli(["auth", "claude", "extra"]);
    expect(code).toBe(2);
    expect(stderr).toContain("usage:");
  });

  test("sessions prints nothing on a fresh state root", async () => {
    const { code, stdout } = await cli(["sessions"]);
    expect(code).toBe(0);
    expect(stdout.trim()).toBe("");
  });

  test("sessions rm requires an id and reports a missing session", async () => {
    expect((await cli(["sessions", "rm"])).stderr).toContain("usage:");
    const missing = await cli(["sessions", "rm", "s_nonexistent"]);
    expect(missing.code).toBe(2);
    expect(missing.stderr).toContain("session not found");
  });

  test("sessions prune validates days and prunes nothing fresh", async () => {
    const empty = await cli(["sessions", "prune"]);
    expect(empty.code).toBe(0);
    expect(empty.stdout).toContain("pruned 0 sessions");
    const bad = await cli(["sessions", "prune", "bogus"]);
    expect(bad.code).toBe(2);
    expect(bad.stderr).toContain("invalid days");
  });

  test("unknown option fails with usage error", async () => {
    const { code, stderr } = await cli(["chat", "--bogus"]);
    expect(code).toBe(2);
    expect(stderr).toContain("unknown option --bogus");
  });
});
