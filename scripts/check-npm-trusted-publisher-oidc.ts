import { readFile } from "node:fs/promises";
import { basename, resolve } from "node:path";

import {
  assertNpmPublisherIdentity,
  runNpmPublisher,
} from "./npm-publisher-boundary.ts";
import {
  releaseDistribution,
  releasePackageForName,
} from "./release-distribution-policy.ts";

// Non-publishing proof that this exact release run can exchange GitHub OIDC
// identity for npm trust before any bytes are published. A real publication
// must never be the first evidence that trusted publishing is configured.
const argument = process.argv[2];
if (argument === undefined || process.argv.length > 3) {
  throw new Error("Usage: check-npm-trusted-publisher-oidc.ts ARTIFACT.tgz");
}
const tarball = resolve(argument);
const manifest = JSON.parse(
  await readFile(resolve(import.meta.dir, "..", "package.json"), "utf8"),
) as Readonly<{
  license?: unknown;
  name?: unknown;
  publishConfig?: Readonly<
    { access?: unknown; provenance?: unknown; registry?: unknown }
  >;
  repository?: Readonly<{ type?: unknown; url?: unknown }>;
  version?: unknown;
}>;
if (
  typeof manifest.name !== "string" ||
  manifest.license !== "MIT" ||
  typeof manifest.version !== "string" ||
  manifest.publishConfig?.access !== "public" ||
  manifest.publishConfig.provenance !== true ||
  manifest.publishConfig.registry !== "https://registry.npmjs.org"
) {
  throw new Error(
    "The public CLI package identity or publication policy is invalid.",
  );
}
const releasePackage = releasePackageForName(manifest.name);
if (
  manifest.repository?.type !== "git" ||
  manifest.repository.url !== `git+https://github.com/${releasePackage.repository}.git`
) {
  throw new Error("The public package repository does not bind the provenance repository.");
}
const distribution = releaseDistribution(releasePackage);
if (basename(tarball) !== distribution.releaseArchiveName(manifest.version)) {
  throw new Error("npm trusted-publisher preflight received the wrong release artifact name.");
}
const verifiedTag = process.env.VERIFIED_TAG;
const verifiedSha = process.env.VERIFIED_SHA;
if (verifiedTag === undefined || verifiedSha === undefined) {
  throw new Error("npm trusted-publisher preflight requires verified release identity.");
}
if (verifiedTag !== `${releasePackage.tagPrefix}${manifest.version}`) {
  throw new Error(
    `npm trusted-publisher preflight requires verified tag ${releasePackage.tagPrefix}${manifest.version}.`,
  );
}
assertNpmPublisherIdentity(process.env, verifiedTag, verifiedSha);
const result = await runNpmPublisher({ dryRun: true, source: process.env, tarball });
if (result.exitCode !== 0 || !result.trustedExchangeProven || result.failure !== null) {
  throw new Error(`npm trusted-publisher OIDC preflight failed (${result.failure ?? "unclassified"}).`);
}
console.log("npm trusted-publisher OIDC exchange preflight passed without publication.");
