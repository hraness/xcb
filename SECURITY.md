# Security

xcb is in development. Native Claude, Codex, and Devin adapters remain subject
to exact-binary admission and per-run boundary verification. The native Codex
0.156.1 and Devin 3000.11.3/3000.11.1/3000.10.31 candidates currently require
macOS. Live acceptance is account/model/build specific and separate from
credential-free boundary proof. Devin checks model availability against the
connected account's fresh catalog at launch. The TypeScript compatibility CLI's
Codex and Devin task routes remain unqualified and disabled. A model catalog,
successful metadata probe, or synthetic fixture is not a production security
attestation.

The execution boundary keeps model-facing tools closed and bounded, credentials
outside workspaces, and provider accounts under exclusive host custody. Report
custody takeover, unbounded input, ambient network or credential access,
confinement escapes, unsafe replay, and process-ownership confusion.

## Reporting

Open a private security advisory on
[hraness/xcb](https://github.com/hraness/xcb/security/advisories/new), or use the
maintainer contact listed on the organization profile. Include a minimal
reproduction when possible. Remove account keys, private paths, provider state,
and transcript contents from diagnostic attachments.

Do not open a public issue for an unpatched vulnerability.
