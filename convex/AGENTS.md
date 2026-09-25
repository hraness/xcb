# Contents

- `convex/` is the xcb relay deployment: a thin instantiation of
  `@hraness/relay` with namespace `xcb.relay.v1`, the xcb command union,
  `daemon`/`controller` device classes, closed sign-up with a one-shot
  bootstrap capability, and env-selected OTP email delivery.

# Guidelines

- The contract lives in `hraness/relay`; this directory only instantiates
  it. Backend behavior changes belong upstream.
- Closed sign-up is deliberate: first admission goes through the
  `XCB_RELAY_BOOTSTRAP` capability, later owners join by issued invite.
- Local development targets only the anonymous deployment selected by
  `scripts/convex-local.ts`. Never commit deploy credentials or point the
  directory at a production deployment from a dev shell.
- The command union (`xcb.relay.v1` kinds) is frozen in
  `docs/plans/remote-access.md`; changing it is a contract change.
