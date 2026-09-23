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
  Fits are MAP estimates anchored on generation 0; promotion requires held-out
  gain on a stable hash split with at least five examples per class.
- Labels come from behavior (continue after completion, model escalation,
  moving on) and explicit `xcb reflex label`; explicit labels outrank inferred
  ones. Only numeric features are stored.
- Modes `off` / `observe` / `active` per reflex. Route ships active (generation
  0 is behavior-identical); settle ships observe.
- Seams: a new reflex is a `Reflex` variant, heads with a prior, a feature
  function, a program and a call site. Operators can replace a program via
  `<state>/reflexes/<name>.algal.json`, admitted only if effect-free.

## Safety

Reflexes refine decisions inside the existing envelope. Route chooses only
among admitted eligible routes and cannot demote the substantial-prompt floor.
Settle continuation runs only after every deterministic continuation gate
passes, and the judge keeps its veto. Any reflex failure falls back to
pre-reflex behavior.

## Next

- Enable settle `active` by default once operators' holdouts show held-out
  positives above the promotion floor.
- Add reflexes for context elision and account preference using the same
  ledger and promotion rule.
- Consider proposing program variants (not only parameters) and evaluating them
  on the same holdout, still admitted only if effect-free.
