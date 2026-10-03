import { expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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
  expect(result.error, result.error?.message).toBeUndefined();
  expect({ status: result.status, stderr: result.stderr }).toEqual({ status: 0, stderr: expect.stringContaining("OK") });
}, 60_000);

test("release signing credentials stay out of build jobs and unsigned assets cannot be published", () => {
  const workflow = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");
  const jobs = Bun.YAML.parse(workflow) as { jobs: Record<string, any> };
  const signing = jobs.jobs.macos_sign;
  const submitting = jobs.jobs.macos_submit;
  expect(submitting.needs).toEqual(["verify", "macos_build"]);
  expect(signing.needs).toEqual(["verify", "macos_build", "macos_submit"]);
  expect(submitting.environment).toBe("xcb-apple-release");
  expect(signing.environment).toBe("xcb-apple-release");
  expect(signing["timeout-minutes"]).toBeLessThanOrEqual(30);
  expect(jobs.jobs.publish_github.needs).toContain("macos_sign");
  for (const [name, job] of Object.entries(jobs.jobs)) {
    if (!["macos_sign", "macos_submit"].includes(name)) expect(JSON.stringify(job)).not.toContain("secrets.APPLE_");
  }
  expect(JSON.stringify(jobs.jobs.native_artifact.strategy.matrix)).not.toContain("darwin-aarch64");
  const unsigned = jobs.jobs.macos_build.steps.find((step: any) => step.id === "unsigned");
  expect(unsigned.with.name).toBe("xcb-unsigned-darwin-aarch64-${{ github.run_attempt }}");
  expect(unsigned.with.name).not.toMatch(/^xcb-native-/);
  const steps = signing.steps as Record<string, any>[];
  const secretStep = steps.findIndex((step) => JSON.stringify(step.env ?? {}).includes("secrets.APPLE_"));
  expect(secretStep).toBeGreaterThan(0);
  expect(steps.filter((step) => JSON.stringify(step.env ?? {}).includes("secrets.APPLE_"))).toHaveLength(1);
  expect(steps[secretStep]?.run).toContain("sign-macos-release.py finalize");
  expect(JSON.stringify(steps)).not.toContain("secrets.APPLE_DEVELOPER_ID");
  expect(JSON.stringify(submitting.steps)).not.toMatch(/check-native-archive|attest-build-provenance|signed-artifacts/);
  const submitSteps = submitting.steps as Record<string, any>[];
  const prepare = submitSteps.findIndex(step => JSON.stringify(step.env ?? {}).includes("secrets.APPLE_"));
  expect(submitSteps.filter(step => JSON.stringify(step.env ?? {}).includes("secrets.APPLE_"))).toHaveLength(1);
  expect(submitSteps[prepare]?.run).toContain("sign-macos-release.py submit");
  expect(submitSteps[prepare + 1]?.if).toBe("always()");
  expect(submitSteps[prepare + 1]?.run).toContain("sign-macos-release.py cleanup");
  const candidate = submitSteps[prepare + 2];
  expect(candidate?.id).toBe("candidate");
  expect(candidate?.with.name).toBe("xcb-apple-candidate-${{ github.run_attempt }}");
  expect(candidate?.with.path.split("\n").filter(Boolean)).toEqual([
    "${{ runner.temp }}/xcb-apple-candidate/notarization.zip", "${{ runner.temp }}/xcb-apple-candidate/receipt.json",
  ]);
  expect(candidate?.with["retention-days"]).toBe(30);
  expect(submitting.outputs.artifact_id).toBe("${{ steps.candidate.outputs.artifact-id }}");
  expect(submitting.outputs.artifact_digest).toBe("${{ steps.candidate.outputs.artifact-digest }}");
  const original = steps.find(step => step.id === "candidate_input");
  expect(original?.env.ARTIFACT_ID).toBe("${{ needs.macos_submit.outputs.artifact_id }}");
  expect(original?.env.ARTIFACT_DIGEST).toBe("${{ needs.macos_submit.outputs.artifact_digest }}");
  expect(steps[secretStep]?.env.CANDIDATE_PRODUCER_ATTEMPT).toBe("${{ steps.candidate_input.outputs.producer_attempt }}");
  expect(original?.run).not.toContain("max_by");
  expect(steps[secretStep + 1]?.if).toBe("always()");
  expect(steps[secretStep + 1]?.run).toContain("sign-macos-release.py cleanup");
  expect(steps[secretStep + 2]?.id).toBe("signed_bytes");
  expect(steps[secretStep + 3]?.run).toContain("check-native-archive.sh");
  expect(steps[secretStep + 4]?.uses).toStartWith("actions/attest-build-provenance@");
  expect(JSON.stringify(steps)).not.toMatch(/cargo |bun install|npm install|build-native\.sh/);
  const bind = submitSteps.find((step) => String(step.run).includes("extract-artifact"));
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
      const admitted = execute();
      expect({ status: admitted.status, stderr: admitted.stderr.toString() }).toEqual({ status: 0, stderr: "" });
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


test("candidate metadata binds the original producer on failed-job retries", () => {
  const workflow = Bun.YAML.parse(readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8")) as { jobs: Record<string, any> };
  const script = workflow.jobs.macos_sign.steps.find((step: any) => step.id === "candidate_input").run;
  const root = mkdtempSync(join(tmpdir(), "xcb-notary-metadata-"));
  try {
    const fake = join(root, "gh");
    writeFileSync(fake, '#!/bin/sh\nprintf "%s\\n" "$*" >> "$GH_CALLS"\ncase "$*" in */zip) printf candidate ;; *) cat "$GH_FIXTURE" ;; esac\n');
    chmodSync(fake, 0o755);
    const metadata = { id: 91, digest: "sha256:" + "d".repeat(64), expired: false,
      workflow_run: { id: 321, head_sha: "a".repeat(40) }, name: "xcb-apple-candidate-1", size_in_bytes: 42 };
    const execute = (value: unknown, attempt = "2") => {
      writeFileSync(join(root, "metadata.json"), JSON.stringify(value));
      writeFileSync(join(root, "calls"), "");
      return spawnSync("/bin/bash", ["-c", script], { encoding: "utf8", timeout: 5000, env: {
        ...process.env, PATH: `${root}:/opt/homebrew/bin:${process.env.PATH ?? "/usr/bin:/bin"}`,
        GH_FIXTURE: join(root, "metadata.json"), GH_CALLS: join(root, "calls"), RUNNER_TEMP: root,
        GITHUB_OUTPUT: join(root, "outputs"), GITHUB_REPOSITORY: "hraness/xcb", GITHUB_RUN_ID: "321",
        GITHUB_RUN_ATTEMPT: attempt, VERIFIED_SHA: "a".repeat(40), ARTIFACT_ID: "91", ARTIFACT_DIGEST: "d".repeat(64),
      } });
    };
    expect(execute(metadata).status).toBe(0);
    expect(readFileSync(join(root, "outputs"), "utf8")).toContain("producer_attempt=1");
    expect(execute({ ...metadata, name: "xcb-apple-candidate-2" }).status).toBe(0);
    for (const replacement of [
      { id: 92 }, { digest: "sha256:" + "e".repeat(64) }, { expired: true },
      { workflow_run: { id: 322, head_sha: "a".repeat(40) } }, { workflow_run: { id: 321, head_sha: "e".repeat(40) } },
      { name: "xcb-apple-candidate-3" }, { name: "xcb-apple-candidate-0" }, { name: "xcb-native-darwin-aarch64-1" },
      { name: "xcb-apple-candidate-01" }, { size_in_bytes: 0 }, { size_in_bytes: 134217729 },
    ]) {
      expect(execute({ ...metadata, ...replacement }).status).not.toBe(0);
      expect(readFileSync(join(root, "calls"), "utf8")).not.toContain("/zip");
    }
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("submission retries refuse any previously admitted or ambiguous producer", () => {
  const workflow = Bun.YAML.parse(readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8")) as { jobs: Record<string, any> };
  const script = workflow.jobs.macos_submit.steps.find((step: any) => step.name === "Refuse another signing or submission attempt").run;
  const root = mkdtempSync(join(tmpdir(), "xcb-notary-prior-"));
  try {
    writeFileSync(join(root, "gh"), '#!/bin/sh\ncat "$GH_FIXTURE"\n');
    chmodSync(join(root, "gh"), 0o755);
    const execute = (value: unknown, attempt = "2") => {
      writeFileSync(join(root, "jobs.json"), JSON.stringify(value));
      return spawnSync("/bin/bash", ["-c", script], { encoding: "utf8", timeout: 5000, env: {
        ...process.env, PATH: `${root}:/opt/homebrew/bin:${process.env.PATH ?? "/usr/bin:/bin"}`,
        GH_FIXTURE: join(root, "jobs.json"), RUNNER_TEMP: root,
        GITHUB_REPOSITORY: "hraness/xcb", GITHUB_RUN_ID: "321", GITHUB_RUN_ATTEMPT: attempt,
      } });
    };
    expect(execute({}, "1").status).toBe(0);
    expect(execute({ total_count: 0, jobs: [] }).status).toBe(0);
    const producer = { name: "Sign and submit exact macOS candidate", conclusion: "skipped" };
    expect(execute({ total_count: 1, jobs: [producer] }).status).toBe(0);
    for (const conclusion of ["success", "failure", "cancelled", null, "timed_out"]) {
      expect(execute({ total_count: 1, jobs: [{ ...producer, conclusion }] }).status).not.toBe(0);
    }
    for (const value of [{}, { total_count: 101, jobs: [] }, { total_count: 2, jobs: [producer] },
      { total_count: 2, jobs: [producer, producer] }]) {
      expect(execute(value).status).not.toBe(0);
    }
    expect(execute({ total_count: 0, jobs: [] }, "101").status).not.toBe(0);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
