#!/usr/bin/env bun
/** The only way to run Convex locally for a relay deployment: the anonymous local backend
 * (`anonymous:anonymous-agent`, loopback URLs, state under `.convex/`).
 * Before any subprocess starts it refuses deploy keys, access tokens,
 * self-hosted or production selectors, non-loopback Convex URLs, and
 * unsupported arguments from the ambient environment, `.env`, and
 * `.env.local`, so local work can never reach the production deployment.
 * Production deploys happen only through an authenticated `npx convex deploy` from the owner's session. */
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const REPOSITORY_ROOT = fileURLToPath(new URL("../", import.meta.url));
const DEFAULT_ENV_PATH = fileURLToPath(new URL("../.env", import.meta.url));
const DEFAULT_ENV_LOCAL_PATH = fileURLToPath(new URL("../.env.local", import.meta.url));
const CONVEX_BINARY = fileURLToPath(new URL("../node_modules/.bin/convex", import.meta.url));

export const anonymousLocalDeployment = "anonymous:anonymous-agent" as const;

const deploymentCredentialKeys = new Set([
  "CONVEX_ACCESS_TOKEN",
  "CONVEX_DEPLOY_KEY",
  "CONVEX_DEPLOYMENT_TOKEN",
  "CONVEX_OVERRIDE_ACCESS_TOKEN",
  "CONVEX_PRODUCTION_DEPLOYMENT_NAME",
  "CONVEX_PROVISION_HOST",
  "CONVEX_SELF_HOSTED_ADMIN_KEY",
  "CONVEX_SELF_HOSTED_URL",
]);

const convexUrlKeys = new Set([
  "CONVEX_CLOUD_URL",
  "CONVEX_SITE_URL",
  "CONVEX_URL",
  "CONVEX_VERSION_API_ORIGIN",
  "NEXT_PUBLIC_CONVEX_SITE_URL",
  "NEXT_PUBLIC_CONVEX_URL",
  "PUBLIC_CONVEX_SITE_URL",
  "PUBLIC_CONVEX_URL",
  "VITE_CONVEX_SITE_URL",
  "VITE_CONVEX_URL",
]);

const safeDevFlags = new Set(["--help", "--once", "-h"]);
const safeTailLogModes = new Set(["always", "disable", "pause-on-deploy"]);

export type LocalConvexCommand = "dev" | "init";
export type LocalConvexEnvironment = Readonly<Record<string, string | undefined>>;
export type LocalConvexLaunchPlan = {
  readonly command: readonly string[];
  readonly cwd: string;
  readonly environment: Record<string, string | undefined>;
};

type EnvAssignment = { readonly key: string; readonly line: number; readonly value: string };

function refuse(message: string): never {
  throw new Error(`relay local Convex refused ${message}.`);
}

function parseQuotedValue(rawValue: string, quote: "\"" | "'", line: number, fileName: string): string {
  let value = "";
  let escaped = false;
  for (let index = 1; index < rawValue.length; index += 1) {
    const character = rawValue[index];
    if (quote === "\"" && escaped) {
      value += character === "n" ? "\n" : character === "r" ? "\r" : character;
      escaped = false;
      continue;
    }
    if (quote === "\"" && character === "\\") {
      escaped = true;
      continue;
    }
    if (character === quote) {
      const remainder = rawValue.slice(index + 1).trim();
      if (remainder !== "" && !remainder.startsWith("#")) refuse(`malformed ${fileName} line ${line}`);
      return value;
    }
    value += character;
  }
  return refuse(`unterminated ${fileName} line ${line}`);
}

/** Parse dotenv assignments strictly enough that no line can smuggle a
 * selector past validation. */
export function parseDotEnv(contents: string, fileName = ".env"): readonly EnvAssignment[] {
  const assignments: EnvAssignment[] = [];
  const lines = contents.replace(/^\uFEFF/u, "").split(/\r?\n/u);
  for (let index = 0; index < lines.length; index += 1) {
    const lineNumber = index + 1;
    const line = lines[index] ?? "";
    if (line.trim() === "" || line.trimStart().startsWith("#")) continue;
    const match = /^(?:\s*export\s+)?\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$/u.exec(line);
    if (match === null || match[1] === undefined) refuse(`malformed ${fileName} line ${lineNumber}`);
    const rawValue = match[2] ?? "";
    const value = rawValue.startsWith("\"") || rawValue.startsWith("'")
      ? parseQuotedValue(rawValue, rawValue[0] as "\"" | "'", lineNumber, fileName)
      : rawValue.split("#", 1)[0]?.trim() ?? "";
    assignments.push({ key: match[1], line: lineNumber, value });
  }
  return assignments;
}

function isLoopbackConvexUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return (url.protocol === "http:" || url.protocol === "https:")
      && (url.hostname === "127.0.0.1" || url.hostname === "[::1]" || url.hostname === "localhost")
      && url.username === ""
      && url.password === ""
      && url.pathname === "/"
      && url.search === ""
      && url.hash === "";
  } catch {
    return false;
  }
}

function validateEntry(key: string, value: string | undefined, source: string): void {
  if (deploymentCredentialKeys.has(key)) refuse(`${key} from ${source}`);
  if (key === "CONVEX_DEPLOYMENT") {
    if (value !== undefined && value !== "" && value !== anonymousLocalDeployment) refuse(`non-anonymous CONVEX_DEPLOYMENT from ${source}`);
    return;
  }
  if (key === "CONVEX_AGENT_MODE") {
    if (value !== undefined && value !== "" && value !== "anonymous") refuse(`non-anonymous CONVEX_AGENT_MODE from ${source}`);
    return;
  }
  if (key === "CONVEX_ALLOW_ANONYMOUS") {
    if (value !== undefined && value !== "" && value !== "true") refuse(`disabled anonymous mode from ${source}`);
    return;
  }
  if (convexUrlKeys.has(key) && value !== undefined && value !== "" && !isLoopbackConvexUrl(value)) {
    refuse(`non-loopback ${key} from ${source}`);
  }
}

function validateArguments(command: LocalConvexCommand, arguments_: readonly string[]): void {
  if (command === "init") {
    if (arguments_.length > 0) refuse("init arguments");
    return;
  }
  for (let index = 0; index < arguments_.length;) {
    const argument = arguments_[index] ?? "";
    if (safeDevFlags.has(argument)) {
      index += 1;
    } else if (argument === "--tail-logs" && safeTailLogModes.has(arguments_[index + 1] ?? "")) {
      index += 2;
    } else if (argument.startsWith("--tail-logs=") && safeTailLogModes.has(argument.slice("--tail-logs=".length))) {
      index += 1;
    } else {
      refuse("an unsupported dev argument");
    }
  }
}

export function planLocalConvexLaunch(options: {
  readonly arguments?: readonly string[];
  readonly command: LocalConvexCommand;
  readonly envContents?: string;
  readonly envLocalContents?: string;
  readonly environment?: LocalConvexEnvironment;
}): LocalConvexLaunchPlan {
  const arguments_ = options.arguments ?? [];
  const environment = options.environment ?? process.env;
  validateArguments(options.command, arguments_);
  for (const [key, value] of Object.entries(environment)) validateEntry(key, value, "the ambient environment");
  for (const assignment of parseDotEnv(options.envContents ?? "", ".env")) {
    validateEntry(assignment.key, assignment.value, `.env line ${assignment.line}`);
  }
  for (const assignment of parseDotEnv(options.envLocalContents ?? "", ".env.local")) {
    validateEntry(assignment.key, assignment.value, `.env.local line ${assignment.line}`);
  }
  const childEnvironment: Record<string, string | undefined> = { ...environment };
  for (const key of deploymentCredentialKeys) delete childEnvironment[key];
  childEnvironment.CONVEX_AGENT_MODE = "anonymous";
  childEnvironment.CONVEX_ALLOW_ANONYMOUS = "true";
  childEnvironment.CONVEX_DEPLOYMENT = anonymousLocalDeployment;
  return { command: [CONVEX_BINARY, options.command, ...arguments_], cwd: REPOSITORY_ROOT, environment: childEnvironment };
}

async function readOptionalEnvFile(path: string): Promise<string | undefined> {
  try {
    return await readFile(path, "utf8");
  } catch (error: unknown) {
    if (typeof error === "object" && error !== null && "code" in error && error.code === "ENOENT") return undefined;
    throw error;
  }
}

async function main(): Promise<number> {
  const [rawCommand, ...arguments_] = process.argv.slice(2);
  if (rawCommand !== "dev" && rawCommand !== "init") {
    console.error("Usage: bun scripts/convex-local.ts <init|dev> [--once] [--tail-logs <mode>]");
    return 1;
  }
  const [envContents, envLocalContents] = await Promise.all([readOptionalEnvFile(DEFAULT_ENV_PATH), readOptionalEnvFile(DEFAULT_ENV_LOCAL_PATH)]);
  const plan = planLocalConvexLaunch({
    arguments: arguments_,
    command: rawCommand,
    ...(envContents === undefined ? {} : { envContents }),
    ...(envLocalContents === undefined ? {} : { envLocalContents }),
  });
  const child = Bun.spawn([...plan.command], { cwd: plan.cwd, env: plan.environment, stdin: "inherit", stdout: "inherit", stderr: "inherit" });
  return await child.exited;
}

if (import.meta.main) {
  try {
    process.exitCode = await main();
  } catch (error: unknown) {
    console.error(error instanceof Error ? error.message : "relay local Convex refused an invalid launch.");
    process.exitCode = 1;
  }
}
