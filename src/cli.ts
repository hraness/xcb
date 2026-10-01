#!/usr/bin/env node
import { fileURLToPath } from "node:url";
import { runCliUpdate } from "@hraness/cli-update";
import { compatibilityUpdateOptions } from "./cli/update.ts";

const VERSION = "0.16.4";

function reportStartupError(error: unknown): number {
  process.stderr.write(`xcb-compat: ${error instanceof Error ? error.message : "unexpected failure"}\n`);
  return 1;
}

async function execute(argv: readonly string[]): Promise<number> {
  const update = await runCliUpdate(compatibilityUpdateOptions({
    version: VERSION, entrypoint: fileURLToPath(import.meta.url), argv,
  }));
  if (update.handled) return update.exitCode;
  let reportError = reportStartupError;
  try {
    const program = await import("./cli-program.ts");
    reportError = program.reportError;
    return await program.main(argv, VERSION);
  } catch (error) {
    return reportError(error);
  } finally {
    await update.release();
  }
}

execute(process.argv.slice(2)).then((code) => process.exit(code), (error) => process.exit(reportStartupError(error)));
