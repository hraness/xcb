# Native Devin boundary fixture

This credential-free macOS fixture exercises Devin 3000.11.1, the native xcb
stdio MCP helper, the real Rust ACP codec, and the production Seatbelt profile.
Only the network clause changes: the fixture replaces provider TCP 443 and DNS
with one loopback-only fake control plane. It never reads existing Devin account
state, resumes a session, opens authentication, or sends traffic to a provider.

Build the helper and the ignored runtime test using the repository's configured
Rust toolchain (through `host-run` when installed):

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

For a new executable, `--candidate-inventory /absolute/path/to/inventory.json`
binds an explicit candidate version, SHA-256 and complete expected tool schemas
inside this fixture only. Production admission remains unchanged. An inventory
mismatch fails the fixture and retains a bounded `observed-inventory.json` in
the private scenario directory for review; it never accepts the new schemas
automatically. After reviewing any changes and passing all scenarios, add the
exact version/hash pair to production admission, rebuild the helper and fixture,
and repeat without the candidate override before recording release evidence.
Do not run `doctor` or change account state as part of this synthetic procedure.

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

A low-context-budget scenario also exercises Devin's started and completed
compaction notifications during brokered tool calls. It verifies the same
native denials and broker effects while treating compaction as informational:
summaries never become task output, and pending calls retain their custody.

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

The [2026-09-22 receipt](devin-native-3000.11.1-macos-arm64.json) passed all
six scenarios for exact build 3000.11.1. Its complete observed native tool
inventory matches the previously reviewed 3000.10.31 schemas. Candidate checks
passed before admission changed; the recorded receipt then reran the default
admitted path with freshly built helper and fixture binaries. Production keeps
both reviewed version/hash pairs, with no cross-version digest substitutions.
This is credential-free boundary evidence, not live coding acceptance.

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

## Read-only quota diagnosis

The runtime reports the admitted client's `-32011` code (or exact
`resource_exhausted` error kind) as **provider quota or resource limit reached**.
It does not infer a reset time, available balance, or per-model limit. Raw
provider errors remain private.

[Devin's self-serve billing documentation](https://docs.devin.ai/admin/billing/self-serve)
says Pro and Teams full seats share daily and weekly quotas across CLI, Desktop
and cloud sessions; Max has a weekly quota. Allowances refresh on a calendar
basis. [Enterprise usage policies](https://docs.devin.ai/enterprise/features/usage-policies)
can impose shared per-user monthly limits, independently of organization limits,
with resets tied to the contract billing cycle. These are plan rules, not
observations of this account's quota or reset.

The opt-in `devin::wire::tests::metadata_fixture::read_only_account_metadata_under_production_profile`
test executes only the documented `auth status` and `models list --format json`
commands. It requires `XCB_DEVIN_METADATA_SPEC` to name a private, owned JSON
file with absolute `state`, `executable`, and new `output` paths plus an explicit
Devin `account` ID. Run the ignored test through the macOS native scheduler.
It uses exclusive xcb probe custody, a disposable profile, the production
sandbox and independent process/pipe joins. Only fixed flags, numeric quota
fields and a small set of known model IDs can enter its receipt. A complete
model JSON payload is not an entitlement check; empty quota fields mean
unobserved, not zero.

The [read-only metadata receipt](devin-read-only-metadata-macos-arm64.json)
records the latest bounded observations. No inference retries, plan changes or
purchases were made. When these commands do not expose the quota, Devin names
[Settings > Plans or enterprise usage pages](https://docs.devin.ai/admin/billing/usage)
as the authoritative account usage source. An available model catalog alone
is insufficient evidence to retry a different model after a resource limit.
