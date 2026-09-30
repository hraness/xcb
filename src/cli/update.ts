import type { CliUpdateOptions } from "@hraness/cli-update";

/** Product policy only: the shared updater owns installation checks and effects. */
export function compatibilityUpdateOptions(input: Readonly<{
  version: string;
  entrypoint: string;
  argv: readonly string[];
  env?: NodeJS.ProcessEnv;
  stdinIsTTY?: boolean;
  stderrIsTTY?: boolean;
}>): CliUpdateOptions {
  const env = input.env ?? process.env;
  const enabled = (key: string) => ![undefined, "", "0", "false"].includes(env[key]?.toLowerCase());
  const command = input.argv[0];
  // Unknown positional commands are workspace paths in this CLI's chat parser.
  const interactiveCommand = command === undefined
    || ["chat", "run", "resume", "doctor", "--provider", "--model"].includes(command)
    || (!command.startsWith("-") && !["auth", "sessions", "judge", "migrate", "update", "help"].includes(command));
  const effectFree = command !== undefined && ["--help", "help", "-h", "--version", "-v"].includes(command);
  return {
    packageName: "@hraness/xcb",
    version: input.version,
    binName: "xcb-compat",
    entrypoint: input.entrypoint,
    argv: input.argv,
    provider: { kind: "github", repository: "hraness/xcb", assetName: "hraness-xcb-{version}.tgz", channel: "stable" },
    ignoreScripts: true,
    effectFree,
    pinned: env.XCB_VERSION !== undefined,
    nested: enabled("XCB_UPDATE_REENTRY") || enabled("HRANESS_UPDATE_REENTRY"),
    suppressAutomatic: !interactiveCommand
      || !(input.stdinIsTTY ?? process.stdin.isTTY)
      || !(input.stderrIsTTY ?? process.stderr.isTTY)
      || input.argv.includes("--json")
      || ["HRANESS_NO_UPDATE", "XCB_NO_UPDATE", "CI"].some(enabled)
      || (env.HRANESS_AUDIENCE !== undefined && env.HRANESS_AUDIENCE !== "human"),
  };
}
