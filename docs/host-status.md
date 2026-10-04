# Host availability (legacy migration reference)

The Convex host-status endpoint is retired from the active xcb path. This page preserves its wire contract for migration and evidence review; do not deploy a new heartbeat sender or treat this endpoint as the current status surface. The replacement status projection belongs to the Valhalla-backed protocol described in the [north-star vision](vision.md).

An optional, opt-in endpoint that answers "is my machine alive?" — even when
the machine or its connection is down. Each machine you configure publishes
only a bounded id, a display label, task counts, a resource snapshot and its
recent activity — never a hostname, path, credential or task content.

xcb stores and serves the data; it does not ship a page to display it. Any
website can read the public endpoint and render it (the hraness.com status
page is one such consumer).

## Configure the legacy deployment (migration reference only)

1. **Pick your machines.** Up to 100. Each needs a stable id — a lowercase
   slug like `macbook`, `office-mini-2` or `laptop-1` (`[a-z0-9]` plus
   hyphens, at most 32 characters) — and any display label you like (up to
   48 characters, e.g. `Ben's MacBook` or `office mini`). Ids and labels are
   public text; choose names you are comfortable publishing.

2. **Make a token per machine.** Generate a random 64-hex-character token and
   keep only its SHA-256 digest in the server configuration:

   ```sh
   token=$(openssl rand -hex 32)          # goes in the runner's token file
   printf %s "$token" | shasum -a 256     # goes in XCB_HOST_STATUS_KEYS
   ```

   Store each raw token in the runner's private `token_file` (mode `0600`).
   The deployment sees digests only, so the config itself cannot send
   heartbeats.

3. **Configure the deployment.** Set `XCB_HOST_STATUS_KEYS` on the Convex
   deployment to a JSON array, one object per machine, exactly:

   ```json
   [{"id":"macbook","label":"Ben's MacBook","tokenSha256":"<64 hex digest>"}]
   ```

   Duplicate ids or digests, extra fields, or any malformed entry reject the
   whole configuration — the endpoint reports unavailable rather than guess.

4. **Point each machine's runner at the heartbeat endpoint.** On each
   machine, configure the [unattended maintenance
   runner](unattended-maintenance.md):

   ```sh
   python3 scripts/unattended-maintenance.py heartbeat-configure \
     --heartbeat-url https://<deployment>.convex.site/host-status/heartbeat \
     --heartbeat-token-file /ABSOLUTE/private-heartbeat-token
   ```

   The token file holds exactly 64 lowercase hex characters (trailing newline
   OK), mode `0600`. The runner sends an observation every five minutes.

5. **Read status anywhere.** `GET https://<deployment>.convex.site/host-status`
   needs no credential. To show it on a site, proxy it through your own
   endpoint rather than embedding the URL in a page — the hraness site does
   this with a closed-schema proxy that bounds the body and re-checks every
   field.

## Send a heartbeat

`POST` JSON with `Authorization: Bearer <token>` and
`Content-Type: application/json`. The body accepts exactly:

```json
{"version":1,"sequence":1,"health":"ok","sampleAgeSeconds":15,
 "tasks":{"running":1,"queued":4,"needsInput":0,"uncertain":0},
 "resources":{"pressure":"normal","swapUsedBytes":0,"physicalTotalBytes":68719476736,"disksFreeBytes":[359703173120]}}
```

`sequence` is a positive safe integer that increases before each send.
`health` is `ok`, `degraded` or `unknown`. `sampleAgeSeconds` is 0–180.
`tasks` counts are integers 0–10000; `resources.pressure` is `normal`,
`warning`, `critical` or `unknown`. Both objects are optional as a pair —
send both or neither; omitted means "not reported" and public fields show
zeros/unknown. The body is at most 1 KiB. The server identifies the machine
from the token — the body carries no id.

| HTTP status | Meaning |
| --- | --- |
| `204` | Accepted |
| `400` | Invalid JSON fields or values |
| `401` | Missing, invalid or rotated credential |
| `409` | Duplicate or older sequence |
| `413` | Body exceeds 1 KiB |
| `415` | Unsupported content type or encoding |
| `429` | Fewer than 60 seconds since this machine's last accepted write |
| `503` | Configuration, storage or service is unavailable |

Rejected requests never refresh the receipt. Sequence and write commit in one
transaction, so concurrent copies land once. The minimum interval limits
accepted writes to one per minute per machine.

## Read status

`GET /host-status` returns `version`, `configured`, `checkedAt` and
`machines`. Each machine has only `id`, `label`, `lastReceivedAt`, `health`,
`state`, `sampleAgeSeconds`, `tasks`, `resources` and `activity`. Times are
server epoch milliseconds. `activity` is a list of recent
`{observedAt, running, queued, needsInput, uncertain}` samples used for
history displays.

| State | Time since the last accepted receipt |
| --- | --- |
| `online` | At most seven minutes |
| `late` | More than seven and at most fifteen minutes |
| `offline` | More than fifteen minutes |
| `never` | No receipt under the configured credential |

Freshness is calculated per request. Responses may be cached 60 seconds and
carry `X-Robots-Tag: noindex`. A machine with no receipt under its current
credential shows `never`/`unknown` with null ages. A consumer should show an
unavailable result when this endpoint fails, and an unconfigured result when
`configured` is false.

## Bounds and rotation

- At most 100 configured machines; total config at most 32 KiB.
- History is kept for 24 hours at the runner's five-minute cadence. The
  public payload shares one activity-point budget across the fleet: machines
  each keep full 24-hour history up to about 21 machines, then history depth
  shrinks gradually so the response stays bounded (minimum 48 points each).
- Rotating a token starts a new generation: public state becomes `never`
  until the new token's first observation; its sequence may restart at one.
- Removing a machine from config hides its retained receipt and history
  without deleting them. An absent or empty config disables the feature.
- Storage holds only the latest receipt per machine plus bounded history;
  heartbeats carry counts and categories, never task content.
