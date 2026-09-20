# Known quota limits and route selection

Native XCB keeps known Claude account-wide exhaustion separate from short-lived
usage percentages. A current-credential observation of 100% use in `five_hour`
or `seven_day` prevents a new coding turn until the provider-reported reset,
even after the five-minute percentage freshness period. If both windows are
exhausted, the later reset applies. A newer observation of the same window can
supersede the old one. Reaching a reset permits another attempt; it does not
prove that the provider will accept it.

Automatic account selection skips these blocked accounts. An explicit blocked
account choice reports why it cannot start. Saved sessions keep their account
binding. The runtime checks again while acquiring account
ownership, so a second terminal cannot race a newly recorded exhaustion.
Configured continuation retains its existing cleanup, effect, checkpoint and
quota-evidence gates; this change only removes blocked candidates.

`xcb accounts` and `/accounts` show the known retry estimate.
`xcb --json accounts` adds `quotaBlockedUntilMs`, the exact Unix timestamp in
milliseconds, separately from `remainingPercent` and `resetsAtMs`. A null block
means no enforceable observation in this narrow scope; it does not certify
that an account is available. Disabled accounts and unsettled runs still have
their own checks.

Observations bind to the account's current credential generation. Only new
observations recorded while holding that account's exact run or metadata-probe
ownership can adopt the generation-bound quota pool. Credential replacement
makes the old pool inapplicable; ordinary refresh with unchanged credentials
retains its binding. Legacy observations remain preserved but cannot impose
this block. Refreshing metadata can supply newer evidence without starting a
coding turn:

```sh
xcb accounts refresh ACCOUNT
```

This first slice does not infer account-wide limits from Claude model-specific
windows, arbitrary Codex quota buckets, or Devin resource-exhaustion errors.
It does not persist unknown-reset denials or authentication health. Existing
percentage summaries remain telemetry, not proof of model-specific availability.

The separation of account health from active work and selection was informed by
[Underclass's routing and health design](https://github.com/ghuntley/underclass/tree/a0ed73d732e5230657595ab6803c182aea93d792).
XCB retains its own custody and provider contracts; no Underclass source code
was copied.
