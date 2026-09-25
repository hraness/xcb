# Publishing

The xcb pipeline publishes `@hraness/xcb` and native binaries from one tag
channel. Releases tagged v0.3.0 and earlier are AgentMixer. This document
describes the release contract, not publication evidence; the release assets
and the site publication datum below record what is actually published.
An immutable annotated `v<version>` tag at a commit in current `main` history
(every `main` commit was admitted by the `Required` check) is a release request. The tag version must equal `package.json`'s
`version`; no other tag shape is admitted.

## Release contract

`.github/workflows/release.yml` runs the whole pipeline. Its jobs:

1. **Resolve release identity.** Confirms the push is one exact `v*` tag,
   resolves the tag to its commit, proves the commit is an ancestor of exact
   advertised `main`, and proves the tag is the newest advertised stable tag.
   Every later job checks out this verified commit; the native builds start
   from it immediately, in parallel with the source verification below.
2. **Verify release source.** Stages the dependency-free release writers from
   the verified source and preserves them as workflow artifacts so later jobs
   execute those exact bytes, not a fresh checkout. Runs the complete
   `bun run check` gate, then builds `dist/` and packs the one release tarball
   with `npm pack --ignore-scripts`. Writes `SHA256SUMS` over the tarball and
   preserves both as artifacts.
3. **Exact tarball install.** On Ubuntu and macOS, downloads the release bytes
   by numeric artifact ID, verifies the checksum, and runs the packed-package
   smoke check: the tarball installs into an isolated consumer and the public
   entry executes — including an account-lease custody round trip — under both
   Bun and Node.
4. **Native binary build.** On Ubuntu and macOS, checks out the verified tag
   commit, builds `xcb` with the pinned Rust toolchain through
   `scripts/build-native.sh` with no dependency cache, and re-admits the
   packaged archive exactly like the installer through
   `scripts/check-native-archive.sh`: exactly one regular `xcb` member, a
   matching `.sha256`, and the extracted binary reporting `xcb <version>`. It
   then attests each tarball's build provenance
   (`actions/attest-build-provenance`; this is the only job holding
   `attestations: write` and `id-token: write`) and preserves
   `xcb-<version>-<os>-<arch>.tar.gz` plus its adjacent `.sha256` checksum as
   run-bound workflow artifacts.
5. **Publish immutable GitHub Release.** The only job holding
   `contents: write`. Re-verifies every downloaded native archive against its
   adjacent checksum, then creates the immutable Latest GitHub Release
   carrying the exact tarball, `SHA256SUMS`, and every native
   archive/checksum pair, and proves it back.
6. **GitHub parity and pre-npm admission.** Proves the immutable GitHub
   Release bytes — tarball, `SHA256SUMS`, and the exact native asset set —
   match the reviewed workflow artifacts. When `vars.XCB_PUBLISH_NPM` is
   `true` it also admits the npm retry state: absent, or an exact same-run
   retry only. When the variable is unset this parity check is the terminal
   artifact gate and no npm state exists.
7. **Publish npm.** Runs only when the repository variable
   `XCB_PUBLISH_NPM` is `true`. Downloads the exact bytes and the reviewed
   dependency-free npm writer, rechecks the checksum, and publishes through
   npm OIDC trusted publishing with provenance. No npm token exists anywhere
   in the pipeline.
8. **Admission.** Runs in both modes. Always verifies the exact annotated
   tag, reviewed-main ancestry, immutable Latest GitHub Release, exact
   tarball/`SHA256SUMS` bytes, and every native pair's digest, size, and
   adjacent checksum. When `XCB_PUBLISH_NPM` is `true` it additionally
   verifies the exact npm registry version, repository, bytes, and Sigstore
   provenance — the provenance certificate must bind this repository, this
   workflow, this tag, and this run — and requires the npm writer's same-run
   completion record.

A failed or interrupted run leaves quarantine, not retry authority: the
pre-npm job only admits a retry that is an exact continuation of the same
run.

### npm publication mode

`XCB_PUBLISH_NPM` is a repository variable, not a secret. When it is unset or
any value other than `true`, the run is a native-only release: `publish_npm`
and the npm retry-state admission are skipped, the GitHub Release plus its
exact-byte parity check is the terminal artifact gate, and `admit` verifies
only the GitHub surface (`NPM_WRITER_RESULT_REQUIRED=false`). A skipped npm
job is not a failure, so the run stays green. Set the variable to `true`
only after the `@hraness/xcb` trusted publisher is configured on npm; before
that the OIDC publish would hard-fail after the immutable GitHub Release is
already public.

## Repository protections

- `main` delivery is protected by the organization "Protect main delivery"
  ruleset: changes arrive through pull requests that auto-merge once the
  required `Required` check passes; no human approval is requested.
- `v*` tags are protected by the organization "Immutable version tags"
  ruleset: a tag names one commit forever.
- Release-critical paths (workflows, release scripts, `package.json`,
  `bun.lock`, this document) are owned in `.github/CODEOWNERS`.
- The former scoped tag namespaces (`agentmixer-v*`, `agentrouter-v*`,
  `xcb-v*`) and the former `hraness/textbutler` repository identity are
  rejected by the release checks on purpose.

## Site deployment

`site/` deploys to Vercel as `xcb.sh` through the standard Git
integration on `main`. The site is informational only; it carries no product
runtime and no release authority.

## Native binary release

The `native_artifact` job inside `.github/workflows/release.yml` builds `xcb`
for Ubuntu and macOS from the verified tag commit through
`scripts/build-native.sh`. Each archive is admitted twice with the installer's
own rules — once in `build-native.sh`, once as a separate workflow step through
`scripts/check-native-archive.sh` — which require exactly one regular `xcb`
member (no AppleDouble companions or extended attributes), a matching
`.sha256`, and an extracted binary reporting `xcb <version>`. Each tarball's
build provenance is attested with `actions/attest-build-provenance`; verify a
downloaded archive with:

```sh
gh attestation verify xcb-<version>-<os>-<arch>.tar.gz -R hraness/xcb
```

The publish job attaches `xcb-<version>-<os>-<arch>.tar.gz` and its adjacent
`.sha256` checksum to the release draft alongside the package tarball and
`SHA256SUMS`; all assets become immutable together when the release is
published. This folding is required, not cosmetic: a published GitHub Release
is immutable, so assets cannot be added afterward — the former
`release-native.yml` `workflow_run` follower could neither see the tag ref nor
extend the finalized release, and has been removed. Native artifacts are not
published to npm; they are a separate release surface alongside the
`@hraness/xcb` compatibility package.

## Site publication datum

`site/published-release.json` is the one source of truth for the release state
the site shows. Its four fields — `version`, `verificationRun`, `archiveUrl`,
and `native` — stay null until a verified xcb release exists. In that state
every public page renders the honest "no native release is published yet;
install from source" copy and offers no download. `site/app/publication.ts`
parses the datum and rejects any other shape at build time.

After public release verification passes, set:

- `version`: the exact stable version, no `v` prefix. It must not exceed the
  source version in `package.json`.
- `verificationRun`: the successful
  `https://github.com/hraness/xcb/actions/runs/<run-id>` URL.
- `archiveUrl`: the existing
  `https://github.com/hraness/xcb/releases/download/v<version>/hraness-xcb-<version>.tgz`
  compatibility package asset, or `null` when the release carries none.
- `native`: a list with at most one entry per built platform, each
  `{ "platform", "url", "sha256Url" }`. Admitted platforms are
  `darwin-aarch64` and `linux-x86_64`; `url` must be the exact
  `.../v<version>/xcb-<version>-<platform>.tar.gz` asset and `sha256Url` that
  URL plus `.sha256`. Omit a platform whose asset does not exist; the site
  then says it must be built from source.

At least one of `archiveUrl` or a native entry is required once `version` is
set. The site then renders "latest verified release: v<version>" with the
per-platform native download and checksum links and the verification run.

Verify the actual assets — the packed manifest (`name: @hraness/xcb`, matching
version), each native archive's checksum, and provenance — before changing the
datum. A pre-rename AgentMixer release cannot satisfy this contract: do not
invent an xcb asset URL from its version number. The site validates every
explicit asset coordinate and never rewrites README installation commands to
another release version. Keep all fields null if publication or verification is
incomplete. `site/tests/fixtures/published-release.json` exercises the
published state in tests without claiming a release. Regenerate the README with
`cd site && bun run sync:readme`, then run `bun run check` before deploying.
A compatibility archive does not prove native artifacts exist, and a native
entry for one platform does not prove another platform's asset exists.
