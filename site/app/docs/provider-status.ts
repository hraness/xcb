/**
 * Supported provider builds and each provider's status, rendered by the docs
 * and llms.txt from this one module. A test checks every build listed here
 * against qualified-builds.json and the runtime constants.
 */
export const supportedBuilds = {
  /** Claude Code within major version 2, at or above this floor. */
  claudeMinimum: "2.1.268",
  codex: ["0.158.0", "0.157.1", "0.156.1"],
  devin: ["3000.11.3", "3000.11.1", "3000.10.31"],
} as const;

/** One sentence per provider. Change a status here, and in README.md. */
export const providerStatus = {
  claude: "Claude's coding workflow passed on macOS ARM64 with the tested account.",
  codex: "Codex CLI 0.158.0, 0.157.1, and 0.156.1 pass xcb's sandbox and tool checks on macOS ARM64. A signed-in coding session was last confirmed on an older build.",
  devin: "Devin's coding workflow passed on macOS ARM64 with the tested account and Devin CLI 3000.11.3.",
} as const;
