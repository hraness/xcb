import type { LaunchFacts, LaunchStatus } from "@hraness/design-kit/launch";

import { publishedRelease, type PublishedRelease } from "../publication";

/**
 * The launch status label, from the published release record
 * (site/published-release.json). With no release it is "Preview".
 */
export function launchStatusFor(release: PublishedRelease | null): LaunchStatus {
  return release === null ? "Preview" : (`Latest release: v${release.version}` as LaunchStatus);
}

export const LAUNCH_STATUS: LaunchStatus = launchStatusFor(publishedRelease);

/**
 * Every number the launch post, its social kit, and the film captions use,
 * each typed once with the record it comes from. tests/launch.test.ts reads
 * those records and fails when a value here drifts from them.
 */
export const launchFacts = {
  providers: {
    value: "two",
    source: "README.md Providers table: Claude and Codex",
  },
  meterMaxAge: {
    value: "five minutes",
    source: "docs/quota-routing.md: usage measurements must be at most five minutes old to count",
  },
  claudeWindows: {
    value: "5-hour and 7-day",
    source: "docs/quota-routing.md: Claude uses the five_hour and seven_day windows",
  },
  importWindow: {
    value: "24 hours",
    source: "README.md: import work active in the last 24 hours",
  },
  status: {
    value: LAUNCH_STATUS,
    source: "site/published-release.json version, through launchStatusFor",
  },
} as const satisfies LaunchFacts;

export type LaunchFactKey = keyof typeof launchFacts;
