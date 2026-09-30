import { expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

test("signer and both Mac admission paths pin the same approved Apple identity", () => {
  const signer = readFileSync(new URL("./sign-macos-release.py", import.meta.url), "utf8");
  const expectedTeam = "8AAP53VTW3";
  const expectedIdentifier = "dev.hraness.xcb";
  expect(signer.match(/^TEAM_ID = "([^"]+)"$/m)?.[1]).toBe(expectedTeam);
  expect(signer.match(/^IDENTIFIER = "([^"]+)"$/m)?.[1]).toBe(expectedIdentifier);
  for (const file of ["install-native.sh", "check-native-archive.sh"]) {
    const script = readFileSync(new URL(`./${file}`, import.meta.url), "utf8");
    expect(script.match(/^  apple_team_id='([^']+)'$/m)?.[1]).toBe(expectedTeam);
    expect(script.match(/^  apple_identifier='([^']+)'$/m)?.[1]).toBe(expectedIdentifier);
  }
});

test("Developer ID signing helper rejects unsafe inputs, cleans credentials, and fails closed", () => {
  const result = spawnSync("python3", ["-I", new URL("./sign-macos-release.test.py", import.meta.url).pathname], {
    encoding: "utf8", timeout: 30_000, maxBuffer: 128 * 1024,
  });
  expect({ status: result.status, stderr: result.stderr }).toEqual({ status: 0, stderr: expect.stringContaining("OK") });
});

test("release signing credentials stay out of build jobs and unsigned assets cannot be published", () => {
  const workflow = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");
  const jobs = Bun.YAML.parse(workflow) as { jobs: Record<string, any> };
  const signing = jobs.jobs.macos_sign;
  expect(signing.needs).toEqual(["verify", "macos_build"]);
  expect(signing.environment).toBe("xcb-apple-release");
  expect(signing["timeout-minutes"]).toBeLessThanOrEqual(30);
  expect(jobs.jobs.publish_github.needs).toContain("macos_sign");
  for (const [name, job] of Object.entries(jobs.jobs)) {
    if (name !== "macos_sign") expect(JSON.stringify(job)).not.toContain("secrets.APPLE_");
  }
  expect(JSON.stringify(jobs.jobs.native_artifact.strategy.matrix)).not.toContain("darwin-aarch64");
  const unsigned = jobs.jobs.macos_build.steps.find((step: any) => step.id === "unsigned");
  expect(unsigned.with.name).toBe("xcb-unsigned-darwin-aarch64-${{ github.run_attempt }}");
  expect(unsigned.with.name).not.toMatch(/^xcb-native-/);
  const steps = signing.steps as Record<string, any>[];
  const secretStep = steps.findIndex((step) => JSON.stringify(step.env ?? {}).includes("secrets.APPLE_"));
  expect(secretStep).toBeGreaterThan(0);
  expect(steps.filter((step) => JSON.stringify(step.env ?? {}).includes("secrets.APPLE_"))).toHaveLength(1);
  expect(steps[secretStep]?.run).toContain("sign-macos-release.py sign");
  expect(steps[secretStep + 1]?.if).toBe("always()");
  expect(steps[secretStep + 1]?.run).toContain("sign-macos-release.py cleanup");
  expect(steps[secretStep + 2]?.id).toBe("signed_bytes");
  expect(steps[secretStep + 3]?.run).toContain("check-native-archive.sh");
  expect(steps[secretStep + 4]?.uses).toStartWith("actions/attest-build-provenance@");
  expect(JSON.stringify(steps)).not.toMatch(/cargo |bun install|npm install|build-native\.sh/);
  const bind = steps.find((step) => String(step.run).includes("extract-artifact"));
  expect(bind?.run).toContain(".workflow_run.head_sha == $sha");
  expect(bind?.run).toContain(".workflow_run.id | tostring");
  expect(bind?.run).toContain('"$ARTIFACT_DIGEST"');
  expect(signing.outputs.artifact_id).toBe("${{ steps.signed.outputs.artifact-id }}");
  expect(signing.outputs.artifact_digest).toBe("${{ steps.signed.outputs.artifact-digest }}");
  expect(signing.outputs.archive_sha256).toBe("${{ steps.signed_bytes.outputs.archive_sha256 }}");
  expect(signing.outputs.checksum_sha256).toBe("${{ steps.signed_bytes.outputs.checksum_sha256 }}");
  const diagnostic = steps.find((step) => step.name === "Preserve non-secret notarization submission diagnostic");
  expect(diagnostic?.if).toBe("always()");
  expect(diagnostic?.with.path).toBe("${{ runner.temp }}/xcb-apple-notarization.json");
  expect(diagnostic?.with.name).not.toMatch(/^xcb-native-/);
  for (const consumer of [jobs.jobs.publish_github, jobs.jobs.pre_npm]) {
    expect(consumer.needs).toContain("macos_sign");
    const selection = consumer.steps.find((step: any) => step.id === "native_ids");
    expect(selection.env.MACOS_ARTIFACT_ID).toBe("${{ needs.macos_sign.outputs.artifact_id }}");
    expect(selection.env.MACOS_ARTIFACT_DIGEST).toBe("${{ needs.macos_sign.outputs.artifact_digest }}");
    expect(selection.run).toContain('.digest == $digest');
    expect(selection.run).toContain('.workflow_run.head_sha == $sha');
    expect(selection.run).toContain('ids=("$MACOS_ARTIFACT_ID")');
    expect(selection.run).toContain('for platform in linux-x86_64 linux-aarch64 windows-x86_64; do');
    const finalBytes = consumer.steps.find((step: any) => step.name === "Require exact signed macOS archive and checksum bytes");
    expect(finalBytes.env.MACOS_ARCHIVE_SHA256).toBe("${{ needs.macos_sign.outputs.archive_sha256 }}");
    expect(finalBytes.env.MACOS_CHECKSUM_SHA256).toBe("${{ needs.macos_sign.outputs.checksum_sha256 }}");
  }
});

test("both release consumers reject substituted or absent signed byte digests", () => {
  const workflow = Bun.YAML.parse(readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8")) as { jobs: Record<string, any> };
  const root = mkdtempSync(join(tmpdir(), "xcb-signing-consumer-"));
  const hash = (value: string) => createHash("sha256").update(value).digest("hex");
  try {
    mkdirSync(join(root, "native-artifacts"));
    const archive = join(root, "native-artifacts/xcb-0.15.2-darwin-aarch64.tar.gz");
    const original = "exact signed Mac bytes";
    const checksum = hash(original) + "\n";
    const env = { ...process.env, VERIFIED_TAG: "v0.15.2", MACOS_ARCHIVE_SHA256: hash(original), MACOS_CHECKSUM_SHA256: hash(checksum) };
    for (const job of [workflow.jobs.publish_github, workflow.jobs.pre_npm]) {
      const script = job.steps.find((step: any) => step.name === "Require exact signed macOS archive and checksum bytes").run;
      const execute = (overrides = {}) => spawnSync("/bin/bash", ["-c", script], { cwd: root, env: { ...env, ...overrides }, timeout: 5_000 });
      writeFileSync(archive, original);
      writeFileSync(archive + ".sha256", checksum);
      expect(execute().status).toBe(0);
      expect(execute({ MACOS_ARCHIVE_SHA256: "" }).status).not.toBe(0);
      expect(execute({ MACOS_CHECKSUM_SHA256: "" }).status).not.toBe(0);
      writeFileSync(archive, "substituted Mac bytes");
      // A replaced archive must fail even when its original checksum file remains.
      expect(execute().status).not.toBe(0);
      writeFileSync(archive + ".sha256", hash("substituted Mac bytes") + "\n");
      expect(execute().status).not.toBe(0);
      writeFileSync(archive, original);
      writeFileSync(archive + ".sha256", checksum.trim());
      expect(execute().status).not.toBe(0);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
