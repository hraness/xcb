# Native Devin boundary fixture

This credential-free macOS fixture exercises Devin 3000.10.31, the native xcb
stdio MCP helper, the real Rust ACP codec, and the production Seatbelt profile.
Only the network clause changes: the fixture replaces provider TCP 443 and DNS
with one loopback-only fake control plane. It never reads existing Devin account
state, resumes a session, opens authentication, or sends traffic to a provider.

Build the helper and the ignored runtime test using the repository's configured
Rust toolchain (through `hra-host-run` when installed):

```sh
cargo build -p xcb-cli --locked
cargo test -p xcb-runtime --locked --lib devin:: --no-run
```

Use the absolute test executable path printed by Cargo. Run with Bun 1.3.14 on
macOS, in the scheduler's `mac-native` lane when installed:

```sh
bun qualification/devin-native.ts \
  --runtime /absolute/path/to/devin \
  --helper /absolute/path/to/target/debug/xcb \
  --test-binary /absolute/path/to/target/debug/deps/xcb_runtime-HASH \
  --output /absolute/path/to/private/evidence.json
```

The harness snapshots the runtime, helper, and test executable, verifies the exact Devin digest, and
creates a new private temporary directory. It leaves that directory for failure
diagnosis; it contains only synthetic fixtures and reproducible binary copies.
The output receipt contains hashes and bounded observations, not local account
paths, credentials, raw requests, or transcripts.

The fixture injects native notebook reads against synthetic account and consumer
files, workspace and stdin symlinks, ordinary reads, shell execution, workspace
and config writes, and web fetches. A positive scenario requires a brokered
workspace write and read to succeed. Each permission-denied native effect gets
a separate disposable process because Devin can end its turn immediately when
permission is denied; the fixture still requires the failed native call to be
observed. Every tool-bearing model request must advertise the exact checked-in
native tool schemas. Automatic title requests advertise no tools and receive
static text without advancing the probes. The harness checks canaries never
reach requests, native effects never appear, the immutable configuration
survives, a PNG prompt is accepted, and every provider process group and bridge
handler joins.

In this exact Devin build, `notebook_read` can run without an ACP permission
callback. Credentials therefore enter through the trusted environment adapter;
persistent account files and the consumer workspace must stay outside the
provider's filesystem grants. The sandbox needs read access to the root
directory itself for startup; that literal grant does not include descendants.
Configuration files remain immutable, including when Devin attempts to persist
its schema version.

This is boundary evidence for the exact executable and host implementation.
It does not demonstrate authentication, account/model availability, billing,
provider service reliability, or real-model task quality. Those require separate
live acceptance before making daily-driver claims.

## Recorded boundary result

The [2026-09-20 receipt](devin-native-3000.10.31-macos-arm64.json) passed all
five scenarios on macOS arm64. It binds the provider, native helper, fixture,
and harness digests and records clean process/bridge joins for every scenario.
The broker write/read pair succeeded, all protected-file canaries remained
private, and the native exec, write, configuration-write, and webfetch probes
failed without effects. This receipt is credential-free boundary evidence;
live authentication and real-model acceptance remain separate.

The pinned client proposes MCP `2025-11-25`; xcb negotiates its supported
`2025-06-18` protocol according to the [MCP lifecycle rules](https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle).
The broker accepts only bounded standard `progressToken` request metadata and
does not treat it as tool arguments or authorization. Receivers may omit
progress notifications under the [MCP progress rules](https://modelcontextprotocol.io/specification/2025-06-18/basic/utilities/progress).

## Live acceptance: provider resource limit

The [2026-09-20 live receipt](devin-live-resource-limit-macos-arm64.json) records
successful authentication-dependent metadata refresh and 385 observed model
choices. A bounded `devin/swe-1-6-fast` broker task then stopped at
`session/prompt` with RPC code `-32011`, before any tool effects. The provider,
CLI process group and output pipes joined, and the synthetic workspace remained
unchanged. This did not pass live task acceptance or activate application access.

The exact installed Devin executable embeds an ACP example mapping `-32011` to
“Quota exhausted.” and `cognition.ai/errorKind: resource_exhausted`. This supports
a provider resource-limit diagnosis; the live error kind, account balance, reset
time and limit scope were not observed. Resolve the provider-side availability
condition before repeating a bounded live acceptance check. The adapter retains
only a fixed diagnostic category and numeric RPC code, never raw provider error
messages or data.
