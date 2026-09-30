import { writeFileSync } from "node:fs";
import { join } from "node:path";

export const fixtureTeamID = "A1B2C3D4E5";
export const fixtureRequirement = `=anchor apple generic and identifier "dev.hraness.xcb" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "${fixtureTeamID}"`;

/** Only private test copies replace the fixed system verifier. These tests
 * exercise how the scripts handle its verdict, not Apple's cryptography. */
export function withMacosVerifierFixture(script: string, stubs: string): string {
  const verifier = join(stubs, "codesign");
  writeFileSync(verifier, `#!/bin/sh
set -eu
printf '%s\\n' "$@" > "$FIXTURE_CODESIGN_LOG"
[ "$#" = 6 ] && [ "$1" = --verify ] && [ "$2" = --strict ] && [ "$3" = --all-architectures ] && [ "$4" = --test-requirement ] || exit 91
[ "$5" = '${fixtureRequirement}' ] || exit 92
[ -f "$6" ] && [ ! -L "$6" ] || exit 93
[ ! -e "$FIXTURE_EXECUTION_LOG" ] || exit 95
case "$FIXTURE_CODESIGN_RESULT" in
  valid) exit 0 ;;
  unsigned|adhoc|wrong-team|wrong-identifier|tampered|untrusted-anchor|wrong-certificate) exit 1 ;;
  *) exit 94 ;;
esac
`, { mode: 0o755 });
  const quoted = `'${verifier.replaceAll("'", "'\\''")}'`;
  return script.replace(/apple_team_id='[^']*'/, `apple_team_id='${fixtureTeamID}'`).replaceAll("/usr/bin/codesign", quoted);
}
