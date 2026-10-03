# Mac release signing

Mac releases starting at 0.15.2 require an Apple Developer ID Application
signature from Apple Team `8AAP53VTW3` with the identifier `dev.hraness.xcb`.
The signing team and identifier
stay the same across versions. Apple can then recognize successive binaries
as the same application rather than identifying each release by its code hash.

Signing does not grant access to Documents or other protected folders. The
first Developer ID build can require an approval when replacing an ad hoc
build. Confirm approval retention with a real background-process upgrade test
before depending on unattended updates.

## Release credentials

The `xcb-apple-release` GitHub environment accepts release tags only. It has
no required reviewers or wait timer. The Mac submission and finalization jobs
use this environment; pull-request checks and native compilation do not receive
its credentials. Only the submission step receives the signing certificate.
Finalization receives the notary API credentials alone.

| Environment secret | Purpose |
| --- | --- |
| `APPLE_DEVELOPER_ID_P12_BASE64` | Base64 encoding of the Developer ID Application certificate and private key in an encrypted PKCS#12 file. |
| `APPLE_DEVELOPER_ID_P12_PASSWORD` | Password for that PKCS#12 file. |
| `APPLE_NOTARY_KEY_P8_BASE64` | Base64 encoding of the Apple API private key used for notarization. |
| `APPLE_NOTARY_KEY_ID` | Identifier of that Apple API key. |
| `APPLE_NOTARY_ISSUER_ID` | Issuer identifier for that Apple API key. |

The expected Apple Team ID is a reviewed source constant, rather than a value
accepted from the downloaded binary. Use a Developer ID Application
certificate belonging to that team. An Apple Development, Apple Distribution,
Developer ID Installer, or locally self-signed certificate does not qualify.
Include Apple's issuing intermediate certificate in the PKCS#12 bundle so a
clean runner can build the certificate chain. A Developer-role App Store
Connect team API key is sufficient for notarization; Admin is unnecessary.

Keep private keys outside the repository, logs, and build artifacts. Transfer
them directly into the environment's encrypted secrets. Retain a protected
backup and track the certificate's expiration. Renewal must preserve the team
and application identifier; the application requirement does not pin one
leaf certificate.

## Build and publication

The native build produces a separate unsigned intermediate archive. Its
workflow artifact name cannot be selected for publication. The submission job
waits for source verification, downloads the same run's identified artifact,
and checks its digest before extracting the single executable.

The submission job imports the certificate into a temporary Keychain on its
isolated runner and appends that Keychain to the user search list so macOS can
find its issuer certificate. It signs the executable with hardened runtime and
a secure timestamp, verifies the Apple certificate chain and identity, then
submits the signed executable to Apple's notary service. Credential cleanup
runs on success, failure, and cancellation where the runner can still execute.
Apple's Keychain deletion command removes the job's Keychain and search-list
entry while preserving other entries. The initial search-list update is not
atomic, so the job must not share its runner with another search-list writer.

After credential cleanup, the job uploads the submitted ZIP and a small JSON
record containing Apple's submission ID, the release identity, and the input
and signed-file hashes. This candidate contains no private keys or service logs.
Its artifact name cannot be selected for publication. The upload must succeed
before the separate finalization job starts waiting for Apple.

Finalization downloads the candidate by the successful submission job's numeric
artifact ID and verifies its digest, originating run, source commit, version,
submission ID, and signed bytes. It waits up to 15 minutes for the original
submission. Publication requires `Accepted`, a successful online notarization
check, and verification that the executable's bytes are unchanged. Native
regression tests exercise Apple's requirement parser alongside the simulated
submission and retry tests.

The final archive and checksum are created from the signed bytes. The release
pipeline verifies and attests that archive before publication. A command-line
executable distributed in a tar archive cannot carry a stapled notarization
ticket; release verification checks Apple's online ticket. Installation checks
the code signature without adding an Apple-network request to each install.

## Waiting for Apple and retrying

An initial submission can remain `In Progress` beyond the 15-minute wait. The
timeout ends the CI attempt; it does not mean Apple rejected the binary. The
release stays unpublished until Apple accepts the submission and all release
checks pass. The candidate and submission record remain available as workflow
artifacts for 30 days.

When the submission job succeeded and finalization timed out, select **Re-run
failed jobs** on that same GitHub Actions run. Finalization downloads the
original candidate and waits on its existing submission ID. It does not sign
the executable or submit it again. Avoid frequent retries while Apple is still
processing the submission; each retry occupies another Mac runner during the
wait.

Do not use **Re-run all jobs** to retry notarization. The submission job refuses
to run again after an earlier attempt started, including when its result is
uncertain. If that job failed, or its candidate artifact is missing, expired,
or does not match the recorded identity, stop and inspect the saved diagnostics.
An accepted submission ID alone cannot reconstruct the submitted binary.

Older release workflows that saved only the submission ID cannot recover the
signed payload through this retry path. Rerunning an immutable release tag
continues to use that tag's workflow and signing code. A new release must use
the corrected pipeline; rebuilding or signing again does not recreate the
original submitted bytes.

## Updating an existing installation

An older `xcb upgrade` runs the installer helper already saved on the Mac.
Installing a new binary with that helper does not add the new signature check.
Run the installer from the new immutable release tag once. The public
`https://xcb.sh/install.sh` entry point selects the latest verified published
release; use it only after its release record includes the signed version.

The new installer verifies the downloaded Mac binary before running its
version command or replacing the installed executable. It also saves the new
installer helper for later updates. Existing tasks and provider state stay
outside the executable installation.

## Testing permission retention

1. Install a verified Developer ID build at the intended permanent path.
2. Start it through the same launchd route used in production and read a
   disposable fixture in Documents. Complete any initial macOS approval.
3. Confirm that process has exited. Replace it with a second verified build
   that has different bytes, the same Team ID, and the same identifier.
4. Repeat the background read without changing privacy settings or approving
   another prompt. Record both build hashes, signing requirements, and results.
5. Remove only the test jobs and fixtures after confirming their processes
   have exited. Preserve the results separately from production state.

Keep updates in notify-only mode until this test passes. This test does not
establish recovery after FileVault unlock, graphical login, or a power loss.

Apple describes persistent code identity in
[Technical Note TN2206](https://developer.apple.com/library/archive/technotes/tn2206/_index.html)
and the release service in
[Notarizing macOS software before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).
