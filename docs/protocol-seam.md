# P1 protocol seam

This note freezes one small, transport-neutral wire slice. It is not a
replacement for the provider protocols and it does not select a Valhalla
transport.

## Inventory

| Concern | Existing contract | Boundary observed | Decision for `xcb.protocol.v1` |
| --- | --- | --- | --- |
| JSON and framing | `src/codex-session.ts`, `crates/xcb-runtime/src/codex.rs`, and `crates/xcb-runtime/src/claude_protocol.rs` drive provider JSON frames; `src/canonical-json.ts` and `crates/xcb-runtime/src/cloud/canonical.rs` provide canonical JSON helpers | Provider envelopes and response IDs are provider-specific and are not xcb's state protocol. | Use one canonical JSON object followed by exactly one LF. The xcb envelope is not JSON-RPC and has no transport dependency. Integer-only JSON values keep Rust and TypeScript bytes identical. |
| Revisions | `src/broker.ts` and `src/cli/workspace.ts` pass opaque `expectedRevision` values; CLI file revisions are bounded stat/inode digests. Rust managed sessions carry numeric revisions (`crates/xcb-runtime/src/store.rs` and `session.rs`), while private records commonly use SHA-256 digests. | There is no one revision algorithm or one global revision namespace yet. | Carry an opaque, bounded revision token or explicit `null` for a genesis/create command. The local owner defines its revision domain; the seam does not pretend a file revision is a task revision. |
| Receipts | TypeScript provider/session/process receipts (`src/codex-session.ts`, `src/codex-process.ts`, `src/browser-session.ts`) and Rust runner/qualification receipts record custody and settlement. Many carry a schema string and `productionQualified: false`. | Receipts are lifecycle/provider records, not a shared command result. | `command/submit` returns only a bounded `receiptId`, status (`accepted` or `replayed`), and resulting revision. Full receipt schemas remain a later slice. |
| Hooks | `crates/xcb-runtime/src/hooks.rs` stores versionless hook JSON, executable SHA-256, disabled-by-default state, and bounded JSON input/output. `src/capabilities.ts` keeps tool contracts explicit. | Hook execution is a trusted host effect, not a wire capability; hook payloads are not canonical frames. | Negotiate `receipt.reference` and `command.submit` only. Hooks stay host-owned and are not exposed by this codec. |
| TypeScript SDK | `src/index.ts` exports the router, task runtime, provider adapters, capability profiles, and provider codecs. `src/runtime.ts` and `src/router.ts` keep account/model selection host-side. | The SDK has provider codecs but no provider-neutral state envelope. Importing it must remain effect-free. | `src/protocol.ts` exposes pure builders, validator, canonical encoder, decoder, and capability negotiation; `src/assurance.ts` adds pure local shadow/replay fixtures and performs no I/O. |
| Codex app-server | `src/codex-session.ts` and `crates/xcb-runtime/src/codex.rs` use `initialize`, `thread/start`, `turn/start`, notifications, provider request IDs, bounded frames, and process-custody receipts. | Codex's JSON-RPC-ish envelope and native request surface are pinned to one admitted provider build. It is useful shape, not xcb's state model. | Keep `requestId` and typed responses, but use `schema`, `kind`, `method`, `expectedRevision`, `idempotencyKey`, `capabilities`, `result`, and bounded `error`. No Codex method is registered in this slice. |

The vector file is `protocol/v1-vectors.json`. Rust's explicit contract is
`crates/xcb-core/src/protocol.rs`; the TypeScript contract is `src/protocol.ts`.
Both are pure codecs. `crates/xcb-core/tests/protocol.rs` and
`test/protocol.test.ts` consume the same golden and negative vectors.

## Frozen slice

The schema identifier is `xcb.protocol.v1`, and the current maximum complete
frame is 64 KiB including its final LF. Requests have these fields:

```text
schema, kind, requestId, method, expectedRevision, idempotencyKey,
capabilities, params
```

`initialize` carries `clientName`, `clientVersion`, and a sorted capability
offer. It has `expectedRevision: null` and `idempotencyKey: null`.
`command/submit` carries a bounded command name and integer-only JSON arguments;
it requires an `idem_...` idempotency key and an explicit `expectedRevision`
string or `null`. Request IDs use the `req_...` namespace. Responses echo the
request ID and method and always carry both `result` and `error` slots (one is
`null`). Successful command results carry a `rcpt_...` receipt reference,
`accepted`/`replayed`, and the resulting revision.

Capabilities currently negotiate the single version and the two names
`command.submit` and `receipt.reference`. Lists are sorted and unique. Unknown
versions or features fail closed rather than being silently ignored.

Errors are a closed code set (`invalid_request`, `unsupported_version`,
`unsupported_capability`, `revision_conflict`, `idempotency_conflict`,
`not_found`, `busy`, `internal`) with only a host-selected message of at most
512 bytes and a boolean `retryable`. No provider error, stack, credential,
path, or arbitrary details field crosses this seam.

Canonical framing rejects whitespace, duplicate keys, batches, embedded CR/LF,
partial lines, and non-UTF-8. Object keys are sorted by JavaScript UTF-16 code
unit order; strings use the `JSON.stringify` escape spelling; numbers are safe
integers only. This is a byte contract, not merely a semantic JSON contract.

## Scope boundary

The P1 wire slice remains deliberately narrow: it does not implement
workspace/event paging, full command receipts, subscriptions, projections, SDK
transport clients, or a Valhalla adapter. P3 now adds a local SQLite event
journal and a pure shadow command/receipt model behind separate tests and
vectors; those additions do not silently expand the `xcb.protocol.v1` wire
schema. See [`docs/assurance.md`](assurance.md) and
`protocol/assurance-v1-vectors.json` for that evidence boundary. Nothing in
these local tests qualifies a provider, transport, remote service, live
account, async schedule, or distributed ordering claim.
