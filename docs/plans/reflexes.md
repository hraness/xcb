# Reflexes: self-improving routing and continuation

## Outcome

The managed harness learns two everyday decisions from the operator's own
behavior: which model tier a task deserves, and whether a completed worker turn
actually finished. Replying "continue" should become unnecessary once the
evidence supports it. The reference is [../reflexes.md](../reflexes.md).

Recovered context: the Devin `router` session (2026-09) fitted ALGAL's
model-router on 306 first prompts (cross-validated AUC 0.74) and explored
response categorization. Its fitted head becomes generation 0 of the route
reflex; its link-determination experiment is not upstreamed.

## Design

- A reflex decision is `program(features, params, evidence)`. Programs are
  effect-free ALGAL organisms (input/const/expr cells, no agent calls), so
  every decision is a replayable receipt pinned by a manifest digest.
- Params are versioned generations of logistic heads in `<state>/reflex.sqlite`.
  Fits are MAP estimates anchored on generation 0. Promotion is a forward
  trial: a challenger fitted on all labels so far must beat the active head
  on the next 48 labels, which neither was fitted on.
- Labels come from behavior and explicit `xcb reflex label`. The reply after a
  completed task is categorized (continue, deliver, approve, handoff done,
  correction, status, other) and labels each settle head; continuations the
  reflex started are labeled by whether they did real work and whether they
  were cancelled. Heavier labels win, so explicit labels outrank inferred
  ones. Only numeric features are stored.
- Modes `off` / `observe` / `active` per reflex, plus a separate `confirm`
  knob for answering go-ahead requests. Route ships active (generation 0 is
  behavior-identical); settle and confirm ship observe.
- Settle v2 (2026-09): priors fitted on 2,428 private operator follow-ups
  (aggregate numbers only in the reference). Tool-call count is the
  strongest signal; a `confirm` head separates "should I merge it?" from
  done; a handoff head was dropped because its base rate is under 2%.
- Seams: a new reflex is a `Reflex` variant, heads with a prior, a feature
  function, a program and a call site. Operators can replace a program via
  `<state>/reflexes/<name>.algal.json`, admitted only if effect-free.

## Safety

Reflexes refine decisions inside the existing envelope. Route chooses only
among admitted eligible routes and cannot demote the substantial-prompt floor.
Settle continuation runs only after every deterministic continuation gate
passes, and the judge keeps its veto. Confirm answers additionally require no
risk or user-action cue, checked in the runtime rather than the replaceable
program. Any reflex failure falls back to
pre-reflex behavior.

## Next

- Enable settle `active` by default once operators' live precision holds at
  the shipped threshold across trials.
- Enable `confirm` only after settle has been active widely, with the judge
  configured.
- Add reflexes for context elision and account preference using the same
  ledger and trial rule.
- Consider proposing program variants (not only parameters) and evaluating
  them with the same forward trials, still admitted only if effect-free.
