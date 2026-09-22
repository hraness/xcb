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

Managed tasks apply a second bounded selection stage after admission. XCB derives
relative quality, cost and latency profiles from observed model identities,
peels non-dominated models into Pareto layers, then scores them for routine,
balanced or complex work. Fresh remaining usage, configured favorites and a
soft workspace-learned provider preference break ties. An explicit opening “Use
Claude/Codex/Devin” directive remains a hard provider constraint. If the judge is
enabled it may choose only from the already eligible top routes; a missing or
failed judgment falls back to the deterministic ordering rather than widening
eligibility. `xcb models tiers --task TEXT` shows the model layers and
`xcb --cwd WORKSPACE models route --task TEXT` previews the admitted route using
the same workspace preferences and provider directive. Preview does not reserve
an account; availability and optional judgment may change before execution. These are relative
routing heuristics, not provider price guarantees; the SWE-2 capability/cost
position follows Cognition’s published
[Pareto analysis](https://cognition.com/blog/swe-2).

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

Official temporary pricing offers are a separate observation class. XCB checks
the bounded public [Devin pricing page](https://devin.ai/pricing) at supervisor
startup and every six hours,
retains its source digest, treats it as stale after 24 hours, and enforces the
advertised end timestamp independently of page freshness. The September 2026
observation annotates the advertised Devin CLI SWE-2 promotion only for known
SWE-2 effort variants. The public offer is conditional on an eligible paid plan;
it does not prove that a connected account qualifies. XCB therefore does not
zero a route’s relative cost or grant a free-price bonus from this observation.
It never qualifies Devin or verifies the user-supplied `--plan` label. Inspect or refresh it with `xcb offers` and
`xcb offers --refresh`.

Managed continuation has one owner. Each supervisor attempt executes exactly
one settled provider turn. Turn/token-limit continuation uses the existing
bounded deterministic policy; semantic continuation may proceed only after the
same safety gates and a positive judge result. A settled account/model quota
failure can choose another admitted Pareto route, excluding routes already
tried by that task. Unsettled or uncertain effects are never failed over.

The separation of account health from active work and selection was informed by
[Underclass's routing and health design](https://github.com/ghuntley/underclass/tree/a0ed73d732e5230657595ab6803c182aea93d792).
XCB retains its own custody and provider contracts; no Underclass source code
was copied.
