import { describe, expect, test } from "bun:test";

import {
  awaitNpmVisibility,
  chooseNpmWriterTransition,
  NPM_VISIBILITY_WINDOW_MILLISECONDS,
  npmVisibilityDelay,
} from "./publish-npm-release";

function clock() {
  let time = 0;
  const sleeps: number[] = [];
  return {
    now: () => time,
    sleep: (milliseconds: number) => {
      sleeps.push(milliseconds);
      time += milliseconds;
      return Promise.resolve();
    },
    sleeps,
  };
}

describe("npm visibility wait", () => {
  test("backs off from five seconds to a one-minute cap", () => {
    expect([1, 2, 3, 4, 5, 6, 50].map(npmVisibilityDelay)).toEqual([
      5_000, 10_000, 20_000, 40_000, 60_000, 60_000, 60_000,
    ]);
    expect(() => npmVisibilityDelay(0)).toThrow("positive attempt");
    expect(NPM_VISIBILITY_WINDOW_MILLISECONDS).toBe(25 * 60_000);
  });

  test("keeps waiting through absent and not-yet-exact metadata", async () => {
    const time = clock();
    const lines: string[] = [];
    const answers: Array<() => string | null> = [
      () => null,
      () => null,
      () => {
        throw new Error("npm release trusted-publisher provenance is missing or invalid.");
      },
      () => "exact",
    ];
    const result = await awaitNpmVisibility({
      log: (line) => lines.push(line),
      lookup: () => Promise.resolve().then(() => answers.shift()?.() ?? null),
      now: time.now,
      sleep: time.sleep,
      windowMilliseconds: NPM_VISIBILITY_WINDOW_MILLISECONDS,
    });
    expect(result.observed).toBe("exact");
    expect(result.attempts).toBe(4);
    expect(time.sleeps).toEqual([5_000, 10_000, 20_000]);
    // Only state changes are logged, each with its elapsed time.
    expect(lines).toEqual([
      "npm visibility poll 1 at 0s: the exact version is not served yet",
      "npm visibility poll 3 at 15s: npm release trusted-publisher provenance is missing or invalid.",
    ]);
  });

  test("stops at the bounded window and reports the last registry error", async () => {
    const time = clock();
    const result = await awaitNpmVisibility({
      log: () => undefined,
      lookup: () => Promise.reject(new Error("npm latest lags")),
      now: time.now,
      sleep: time.sleep,
      windowMilliseconds: 3 * 60_000,
    });
    expect(result.observed).toBeNull();
    expect((result.lastFailure as Error).message).toBe("npm latest lags");
    // 5 + 10 + 20 + 40 + 60 = 135s, then the final sleep is clipped to 45s.
    expect(time.sleeps).toEqual([5_000, 10_000, 20_000, 40_000, 60_000, 45_000]);
    expect(time.now()).toBe(3 * 60_000);
    expect(result.attempts).toBe(7);
  });

  test("a full window of one-minute polls stays well inside the job timeout", async () => {
    const time = clock();
    const result = await awaitNpmVisibility({
      log: () => undefined,
      lookup: () => Promise.resolve(null),
      now: time.now,
      sleep: time.sleep,
      windowMilliseconds: NPM_VISIBILITY_WINDOW_MILLISECONDS,
    });
    expect(result.observed).toBeNull();
    expect(time.now()).toBe(NPM_VISIBILITY_WINDOW_MILLISECONDS);
    expect(result.attempts).toBeLessThan(40);
  });
});

describe("npm writer transition", () => {
  test("publishes only when absent and observes a retry's existing release", () => {
    const base = { currentAttempt: 1, preflightAttempt: 1, preflightState: "absent" as const };
    expect(chooseNpmWriterTransition({ ...base, releaseExists: false })).toBe("publish");
    expect(() => chooseNpmWriterTransition({ ...base, releaseExists: true })).toThrow("ambiguous");
    expect(chooseNpmWriterTransition({ ...base, currentAttempt: 2, releaseExists: true }))
      .toBe("observe_existing");
    expect(chooseNpmWriterTransition({
      currentAttempt: 2,
      preflightAttempt: 2,
      preflightState: "exact_same_run",
      releaseExists: true,
    })).toBe("observe_existing");
  });
});
