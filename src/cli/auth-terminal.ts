import { spawnSync, type ChildProcess } from "node:child_process";

/** A provider inherits fd 0, so a previous raw-mode UI must not leave Enter
 * as an untranslated CR. Keep the original state, including uncommon flags,
 * and restore it only after the provider and its output streams have closed. */
export async function withAuthTerminal<T>(run: () => Promise<T>): Promise<T> {
  if (!process.stdin.isTTY || process.platform === "win32") return run();
  const stty = process.platform === "linux" ? "/usr/bin/stty" : "/bin/stty";
  const invoke = (args: string[]) => spawnSync(stty, args, {
    stdio: [0, "pipe", "pipe"], encoding: "utf8", timeout: 2_000,
    maxBuffer: 4_096, env: { LANG: "C" },
  });
  const snapshot = invoke(["-g"]);
  if (snapshot.status !== 0 || snapshot.error !== undefined || !snapshot.stdout.trim()) {
    throw new Error("AUTH_TERMINAL_STATE_UNAVAILABLE");
  }
  const original = snapshot.stdout.trim(), raw = process.stdin.isRaw === true;
  const flowing = process.stdin.readableFlowing === true;
  if (flowing) process.stdin.pause();
  try {
    process.stdin.setRawMode(false);
    const normal = invoke(["icanon", "isig", "iexten", "icrnl", "-igncr", "-inlcr", "echo"]);
    if (normal.status !== 0 || normal.error !== undefined) throw new Error("AUTH_TERMINAL_NORMALIZE_FAILED");
    return await run();
  } finally {
    // Node's raw-mode bookkeeping and the terminal's exact settings are both
    // restored; setRawMode alone does not preserve arbitrary inherited flags.
    try {
      process.stdin.setRawMode(raw);
    } finally {
      const restored = invoke([original]);
      if (flowing) process.stdin.resume();
      if (restored.status !== 0 || restored.error !== undefined) throw new Error("AUTH_TERMINAL_RESTORE_FAILED");
    }
  }
}

export type AuthChildExit = Readonly<{
  code: number;
  signal: NodeJS.Signals | null;
  cancelled: boolean;
  timedOut: boolean;
}>;

/** Wait for close rather than exit: output may still contain a token, and
 * the child must no longer own the terminal when its original flags return.
 * Cancellation never starts another authentication attempt. */
export function waitForAuthChild(child: ChildProcess, timeoutMs: number): Promise<AuthChildExit> {
  return new Promise((resolve) => {
    let failed = false, cancelled = false, timedOut = false;
    let escalation: ReturnType<typeof setTimeout> | undefined;
    let drain: ReturnType<typeof setTimeout> | undefined;
    const signals = ["SIGINT", "SIGTERM", "SIGHUP"] as const;
    const handlers = signals.map(signal => {
      const handler = () => {
        if (cancelled) return;
        cancelled = true;
        child.kill(signal);
        escalation = setTimeout(() => { child.kill("SIGKILL"); }, 1_000);
      };
      process.on(signal, handler);
      return handler;
    });
    const timeout = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, timeoutMs);
    child.once("error", () => { failed = true; });
    child.once("exit", () => {
      // An exited child can leave pipes inherited by a helper. Stop reading
      // those pipes after a short drain; do not signal any unrelated helper.
      drain = setTimeout(() => { child.stdout?.destroy(); child.stderr?.destroy(); }, 1_000);
    });
    child.once("close", (code, signal) => {
      clearTimeout(timeout);
      clearTimeout(escalation);
      clearTimeout(drain);
      signals.forEach((name, index) => { process.removeListener(name, handlers[index]!); });
      // Some provider UIs consume Ctrl+C in raw mode and exit with the shell
      // convention instead of receiving a signal themselves.
      cancelled ||= code === 130 || code === 143 || code === 129
        || signal === "SIGINT" || signal === "SIGTERM" || signal === "SIGHUP";
      resolve(Object.freeze({ code: failed || cancelled || timedOut ? 1 : code ?? 1, signal, cancelled, timedOut }));
    });
  });
}
