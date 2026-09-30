# Host availability

The optional host status endpoint records when each of two home laptops last
reported to the xcb deployment. A website can read it while both laptops or
their internet connection are down. Each laptop publishes only an anonymous
alias, its current health category and the age of its local observation.

Use this with the [unattended maintenance runner](unattended-maintenance.md).
The runner sends a new observation every five minutes. A received heartbeat
shows that the runner reached the server; `health` separately reports what it
observed about the local supervisor and resources. The relay's existing device
presence remains independent because its network thread can keep running while
the supervisor loop has stopped.

## Configure the deployment

Deploy the backend through the repository's release and deployment process.
The existing relay tables, authentication and encrypted fleet data keep their
current behavior. Host availability uses a separate `xcbHostStatus` table with
at most two rows, one latest receipt for each fixed alias.

Set `XCB_HOST_STATUS_KEYS` to a JSON array of one or two objects with exactly
these fields:

| Field | Value |
| --- | --- |
| `id` | `laptop-1` or `laptop-2` |
| `label` | `laptop 1` or `laptop 2`, matching the id |
| `tokenSha256` | The token's SHA-256 digest, as 64 lowercase hexadecimal characters |

Give each laptop a different randomly generated token of 64 lowercase
hexadecimal characters. Keep its raw token in the runner's private token file,
with mode `0600`. Store only the digest in the deployment environment. Configure
the sender with the exact endpoint
`https://<deployment>.convex.site/host-status/heartbeat`; the website reads
`https://<deployment>.convex.site/host-status` without a credential.

The entire configuration is rejected if either entry is malformed, an alias or
digest is repeated, a label differs from its fixed anonymous name, or there are
more than two entries. An absent setting or empty array disables ingestion and
returns an explicit unconfigured public result.

Rotating a token changes the generation for that alias. Its public state becomes
`never` until the replacement token sends a new observation. The replacement can
restart its sequence at one; it updates the same row and still respects the
minimum time between writes. Removing an alias from configuration hides its
retained receipt. Removing the setting disables the feature without deleting
stored evidence.

## Send a heartbeat

Send a JSON `POST` with `Authorization: Bearer <token>` and
`Content-Type: application/json`. The body accepts exactly:

```json
{"version":1,"sequence":1,"health":"ok","sampleAgeSeconds":15}
```

`sequence` is a positive safe integer that increases before each send attempt.
`health` is `ok`, `degraded` or `unknown`. `sampleAgeSeconds` is an integer from
zero through 180. The body may contain at most 1 KiB. The server identifies the
alias from the token and records its own receipt time.

| HTTP status | Meaning |
| --- | --- |
| `204` | Accepted |
| `400` | Invalid JSON fields or values |
| `401` | Missing, invalid or rotated credential |
| `409` | Duplicate or older sequence |
| `413` | Body exceeds 1 KiB |
| `415` | Unsupported content type or encoding |
| `429` | Fewer than 60 seconds since this alias's last accepted write |
| `503` | Configuration, storage or service is unavailable |

Rejected requests never refresh the receipt time. The sequence check and write
occur in one database transaction, so concurrent copies of a request commit
once. Five-minute sending produces at most 576 accepted writes per day across
two laptops; the server's minimum interval limits accepted writes to one per
minute per alias. It retains no heartbeat history.

## Read availability

`GET /host-status` returns `version`, `configured`, `checkedAt` and `machines`.
Each configured machine contains only `id`, `label`, `lastReceivedAt`, `health`,
`state` and `sampleAgeSeconds`. Times are server milliseconds since the Unix
epoch. An alias with no receipt under its current credential has null receipt
and sample ages, `unknown` health and `never` state.

| State | Time since the last accepted receipt |
| --- | --- |
| `online` | At most seven minutes |
| `late` | More than seven and at most fifteen minutes |
| `offline` | More than fifteen minutes |
| `never` | No receipt under the configured credential |

The action calculates freshness when the request arrives, including when no
database writes have occurred. Public responses may be cached for 60 seconds
and carry `X-Robots-Tag: noindex`. `health` and `sampleAgeSeconds` describe the
last received observation; `state` describes whether that observation is still
arriving on schedule. A website should show an unavailable result when this
endpoint fails, and an unconfigured result when `configured` is false.
