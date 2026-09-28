# Known quota limits and route selection

Automatic routing favors fresh subscription capacity that would otherwise go
unused before a reset. For Claude and Codex, xcb divides the remaining
percentage in each known account-wide window by the hours until that window
resets, then uses the lowest rate across overlapping windows. This lets unused
capacity influence the choice without treating a nearly renewed short window
as permission to exhaust a weekly allowance. The route explanation includes
the rate when it contributed to the choice.

The rate adds a capped preference to the existing model score. It preserves
explicit account, provider, and model choices, known usage blocks, and the
highest-quality tier required by a substantial task. Measurements must be
at most five minutes old and belong to the account's applicable quota pool.
A stale active window makes the rate unknown; unknown capacity receives no
bonus. Devin currently reports no comparable account-wide percentage meter.
Percentages across subscriptions are a routing heuristic, not equal amounts of
work or a promise about provider billing.

This policy draws on the reset-aware scheduling approach in
[llm-quota-router](https://github.com/tonygwu/llm-quota-router/tree/61d952568cdda6b661857cd96f6b9c8fc09a9cdf). xcb keeps its
own provider checks, account ownership, and task-quality rules. No source code
from that project was copied.

Native xcb keeps known account-wide exhaustion separate from short-lived
usage percentages. Claude uses `five_hour` and `seven_day`; Codex uses
`codex.primary` and `codex.secondary`. An applicable observation of 100% use
prevents a new coding turn until the provider-reported reset,
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

Managed tasks then rank the accounts and models that remain. xcb derives
relative quality, cost, and latency profiles from observed model identities and
sorts the models into Pareto layers: a model is in the first layer when no other
model beats it on all three at once. It then scores the layers for routine,
balanced, or complex work. Fresh quota timing, configured favorites and a
soft workspace-learned provider preference adjust the score. An explicit opening “Use
Claude/Codex/Devin” directive remains a hard provider constraint. The optional
judge classifies capability demand through the ALGAL fitted classifier; route
selection then follows deterministic policy within already eligible candidates.
A missing or failed classification uses a deterministic demand estimate without
widening eligibility. `xcb models tiers --task TEXT` shows the model layers and
`xcb --cwd WORKSPACE models route --task TEXT` previews the route using
the same workspace preferences and provider directive. Preview does not reserve
an account; availability and optional classification may change before execution. These are relative
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
this block. Refreshing metadata can supply newer evidence without starting a coding turn
on Claude Code builds that report usage at session start; 2.1.282 reports usage
only on requests, so the next routed turn records the meters instead:

```sh
xcb accounts refresh ACCOUNT
```

This first slice does not infer account-wide limits from Claude model-specific
windows, arbitrary Codex quota buckets, or Devin resource-exhaustion errors.
It does not persist unknown-reset denials or authentication health. Existing
percentage summaries remain telemetry, not proof of model-specific availability.

Official temporary pricing offers are a separate observation class. xcb checks
the bounded public [Devin pricing page](https://devin.ai/pricing) at supervisor
startup and every six hours,
retains its source digest, treats it as stale after 24 hours, and enforces the
advertised end timestamp independently of page freshness. The September 2026
observation annotates the advertised Devin CLI SWE-2 promotion only for known
SWE-2 effort variants. The public offer is conditional on an eligible paid plan;
it does not prove that a connected account qualifies. xcb therefore does not
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
xcb retains its own custody and provider contracts; no Underclass source code
was copied.

## Automatic capability selection

Native managed tasks and unpinned `xcb run` use the same automatic selector.
The optional typed judge asks the six questions from ALGAL's fitted model-router:
kind, difficulty, scope, ambiguity, stakes and frontier demand. The fitted head
combines them with deterministic prompt-shape features. One bounded call supplies
all answers; missing, invalid or timed-out answers use deterministic routing.
The fit predicts one operator's historical model choices, not measured model
quality. Its implementation and provenance are in `task_classifier.rs` and
[ALGAL's model-router documentation](https://github.com/hraness/algal/blob/main/docs/model-router.md).

The fitted head is generation 0 of the `route` [reflex](reflexes.md). The
reflex runs it as an effect-free ALGAL program, records the decision, and
learns later generations from your explicit and implicit tier choices. A
generation is promoted only when a forward trial on labels received after
fitting lowers log loss within the accuracy and AUC guardrails described in
[forward trials](reflexes.md#forward-trials). `xcb reflex rollback route 0`
restores the fitted head.

Score answers use zero-based criterion indices, as specified by the
[TypeSafe API](https://docs.typesafe.ai/api#score-answer). Five criteria therefore
admit indices 0–4; their text labels do not change the numeric scale. The ALGAL
example response fixture includes an out-of-range probability bucket `5` and
is not a live-wire conformance fixture. Native tests preserve the fitted
numeric inputs while using valid probability distributions; invalid bucket
responses fall back instead of weakening judge validation.

A prompt with at least 400 words or 8 KiB requests the highest known quality
among eligible routes independently of classifier availability. This is an
explicit xcb policy, not a claim made by the fitted classifier. Lower price,
quota percentage, favorites and provider preference cannot demote that quality
tier. Explicit provider/model constraints still narrow eligibility first.

If observed usage exhaustion excludes a stronger connected admitted model, the
selected route explains the downgrade. Busy, disconnected and unqualified
routes are not described as quota failures. If a matching admitted route is
quota-blocked and no eligible fallback exists, the CLI reports the usage limit
and managed work enters the attention inbox while waiting for availability.
Unknown account/model availability
remains unknown until the provider validates the route. An observed model's
context-window size is not recorded here, so length-based selection is a quality
policy and does not certify context fit. Provider/session admission and account
custody continue to be checked at execution time.
