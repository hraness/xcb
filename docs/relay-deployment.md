# Relay deployment (legacy migration reference)

The Convex relay is retired from the active xcb path. Keep this document only
for migration inventory and recovery review; new deployments belong to the
Valhalla transport plan in [the north-star vision](vision.md).

This is a reference for running your own Convex relay that
[remote operations](remote-operations.md) connect to. The relay code is this
repository's `convex/` folder, and its design is in
[remote access](plans/remote-access.md).

## Deploy

Deploy code changes from the repository root with your deployment selected:

```sh
CONVEX_DEPLOYMENT=<deployment> npx convex deploy
```

Set the relay's environment with `npx convex env set <KEY> <value>`:

- `JWT_PRIVATE_KEY` and `JWKS`: the RS256 key pair that signs and verifies
  device sessions. Rotate both together, and always set the private key with
  `npx convex env set JWT_PRIVATE_KEY --from-file <pem>`; a multi-line value
  passed inline is stored mangled and breaks signing. After a rotation, devices
  holding an old session recover on their own: the first call fails, one
  forced refresh signs a new session, and the call retries.
- `XCB_RELAY_BOOTSTRAP`: a one-time invite for the first owner. It stops working
  once an owner is verified; set a fresh value only when rebuilding a fleet.
- `XCB_RELAY_EMAIL`: how sign-in codes are delivered: `log`, `resend`, or
  `webhook`. `resend` needs `XCB_RESEND_API_KEY` and `XCB_RESEND_FROM` (a
  branded sender on a verified domain, e.g. `xcb <xcb@auth.hraness.com>`);
  `webhook` needs `XCB_OTP_WEBHOOK_URL` and `XCB_OTP_WEBHOOK_TOKEN`. With
  `log`, codes are printed to the function log, which you read from an
  authenticated Convex session with `npx convex logs`.

The same deployment can optionally receive heartbeats from a bounded fleet of
machines for an external status page. [Host status](host-status.md) describes
its separate credential configuration, bounded public fields and receipt
expiry.
