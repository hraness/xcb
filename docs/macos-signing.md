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
no required reviewers or wait timer. Only the Mac signing job uses this
environment; pull-request checks and native compilation do not receive its
credentials.

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

Keep private keys outside the repository, logs, and build artifacts. Transfer
them directly into the environment's encrypted secrets. Retain a protected
backup and track the certificate's expiration. Renewal must preserve the team
and application identifier; the application requirement does not pin one
leaf certificate.

## Build and publication

The native build produces a separate unsigned intermediate archive. Its
workflow artifact name cannot be selected for publication. The signing job
waits for source verification, downloads the same run's identified artifact,
and checks its digest before extracting the single executable.

The signing job imports the certificate into a temporary Keychain, signs the
executable with hardened runtime and a secure timestamp, and checks its Apple
certificate chain, Team ID, and application identifier. It submits the signed
executable to Apple's notary service and requires an `Accepted` result and a
successful notarization check. Credential cleanup runs on failure as well as
success.

The job records Apple's submission ID and the input and signed-file hashes
before waiting up to 15 minutes for notarization. This small diagnostic remains
available as a workflow artifact after a timeout so the existing submission can
be checked without automatically submitting it again. It contains no private
keys or service logs. A timeout fails the release.

The final archive and checksum are created from the signed bytes. The release
pipeline verifies and attests that archive before publication. A command-line
executable distributed in a tar archive cannot carry a stapled notarization
ticket; release verification checks Apple's online ticket. Installation checks
the code signature without adding an Apple-network request to each install.

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
