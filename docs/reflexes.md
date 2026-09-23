# Reflexes: learned routing and turn categorization

A reflex is a small decision the harness makes many times a day, and that it
can learn from how you respond to it. xcb ships two:

| Reflex | Decides | Heads | Default |
| --- | --- | --- | --- |
| `route` | Frontier or standard model tier for a new task | `judged` (with judge evidence), `plain` (prompt shape and keyword cues only) | `active` |
| `settle` | How a settled worker turn ended: `done`, `stopped_short`, `question`, `needs_action`, `needs_approval`, `blocked`, `interrupted`, `uncertain`, ... | `unfinished` | `observe` |

Auto-continuation is the settle reflex acting: a completed turn categorized as
`stopped_short` is continued in its session when settle is `active` and every
deterministic continuation gate passes.

## Shape

Every reflex decision is

```
decision = program(features, params, evidence)
```

- **Program**: an ALGAL organism with only `input`, `const` and `expr` cells and
  no agent calls. It scores each head and applies the gates. Because it has no
  effects, every decision is a replayable ALGAL receipt, and its manifest digest
  identifies the policy exactly. The shipped programs are
  `crates/xcb-runtime/reflexes/route.algal.json` and `settle.algal.json`.
- **Params**: one logistic head per decision family (bias, named weights,
  threshold), versioned as generations. Learning adds a generation; it never
  edits the program.
- **Features**: deterministic numeric features computed in `xcb_core::reflex`
  from the prompt or the worker's last message and the turn's typed facts.
  The judge's answers, when available, are features too, supplied as program
  input. The reflex never calls a provider itself.
- **Evidence**: typed gate inputs the program reads directly: for route,
  whether the prompt is substantial and the judged request kind; for settle,
  the keyword state, turn-limit and blocker signals.

Generation 0 of each reflex reproduces the behavior xcb had before reflexes:

- `route.judged` holds the coefficients of ALGAL's fitted model-router (306
  first prompts, cross-validated AUC 0.74). The program keeps its
  question/probe kind gate and xcb's substantial-prompt policy (400 words or
  8 KiB always requests the highest quality tier).
- `route.plain` reproduces the keyword fallback: a complexity cue
  (architecture, migration, security, race, refactor, ...) requests frontier.
- `settle.unfinished` is a conservative prior over end-of-turn cues: a
  promised next step ("Next, I'll ..."), a worker that parked itself on an
  external event ("Waiting on the background watch; I'll merge on green"),
  open checklist items, a message that ends on a colon or mid-sentence and
  remaining-work phrases, offset by completion claims and "let me know if you
  want more" offers.

Enabling reflexes therefore changes nothing until your own evidence earns a
promotion.

## How it learns

Decisions are observed; your behavior labels them.

| Signal | Reflex | Label | Weight |
| --- | --- | --- | --- |
| You reply `continue` (or `keep going`, `go on`, `proceed`, ...) to a task that just completed | settle | unfinished | 1.0 |
| You start something else after a task completed | settle | done | 0.5 |
| You ask for a stronger model ("use opus", "better model") after a task | route | frontier | 1.0 |
| You ask for a lighter model ("use sonnet", "cheaper model") | route | standard | 1.0 |
| `xcb reflex label <reflex> <task> <label>` | either | explicit | 1.0 |

An inferred label never overwrites an explicit one. After every 16 labels, and
whenever you run `xcb reflex train`, xcb fits a candidate for each head:

1. Labeled examples are split by a stable hash of their id: one fifth is a
   fixed holdout that is never trained on.
2. The candidate is the MAP fit of the shipped prior given the training
   split (logistic regression with a Gaussian prior centered on generation 0),
   so a few labels cannot move a head far, and repeated passes over the same
   evidence converge instead of drifting.
3. The candidate is promoted only if the holdout has at least 12 examples
   with at least 5 of each class, and on it the candidate lowers log loss by
   at least 0.002 without losing more than 0.02 accuracy or 0.02 AUC.

Promotion appends a generation that records its parent, how many labels it
was trained on, and the holdout comparison that justified it.
`xcb reflex rollback <reflex> <version>` reactivates any recorded generation,
and version 0 restores the prior.

## Measured on one operator's history

Bootstrapped in an isolated state from the router study's corpus (first prompts
from Codex, Claude Code and Devin sessions, labeled by the model tier the
operator picked) and from Claude Code transcripts (a completed assistant turn
labeled unfinished when the next message was "continue" or similar; provider
errors and usage-limit stops excluded). Numbers are on the stable holdout
split, which training never sees.

| Head | Labels | Holdout | Generation 0 | After `train` |
| --- | --- | --- | --- | --- |
| `route.judged` | 317 (114 frontier) | 58 | AUC 0.781 · log loss 0.490 | not promoted: no held-out gain |
| `route.plain` | 320 (116 frontier) | 70 | AUC 0.442 · log loss 0.770 | promoted: AUC 0.650 · log loss 0.671 |
| `settle.unfinished` | 523 (30 unfinished) | 89 | AUC 0.713 · log loss 0.328 | not promoted: 2 held-out positives |

The keyword fallback carries no signal for this operator's tier choice; the
learned plain head recovers part of that signal from prompt shape alone
(an imperative opening, resume language, distinct action verbs), without a
judge call. The judged head is already fitted to the same operator, so local
evidence does not move it. Of the turns followed by "continue", most were
workers that ended their turn waiting on CI or a background job; that
observation is what the `awaiting` feature encodes. The settle evidence is
still too thin to promote, which is the gate doing its job.

These labels describe preference, not capability: the route reflex learns
which tier you would pick.

## Replacing a program

Put an organism at `<state>/reflexes/route.algal.json` or
`<state>/reflexes/settle.algal.json` to change the decision logic itself, for
example a new gate or a different way to combine heads. `xcb reflex check
<file>` validates it. A program is admitted only if it has no effectful cells,
no agent calls, the interface inputs `features`, `params` and `evidence`, and
the output `decision`. A rejected program is reported by `xcb reflex status`
and the shipped program runs instead. Every observation records the digest of
the program that made it.

## Commands

```
xcb reflex                      # status of both reflexes
xcb reflex status route         # generation, program digest, labels, holdout metrics
xcb reflex train settle         # fit and maybe promote now
xcb reflex label route t_… frontier
xcb reflex label settle t_… unfinished
xcb reflex rollback route 0     # back to the shipped prior
xcb reflex import route corpus.jsonl
xcb reflex check my-route.algal.json
```

`import` bootstraps from your own history. Each line is
`{"id", "text", "label"[, "weight"][, "judge"]}`, where `judge` carries
`difficulty`, `scope`, `ambiguity`, `stakes` (criterion indices 0–4) and
`frontier` (0–1) for the judged route head. Only the derived features are
stored.

## Configuration

`config.json` `extensions.reflexes`:

```json
{ "route": "active", "settle": "observe", "learn": true }
```

- `off`: the reflex does not run; routing and continuation behave as before.
- `observe`: decisions are recorded and learned from, but do not act. Settle
  categories are still shown on tasks.
- `active`: the decision acts. For route it sets the tier; for settle a
  `stopped_short` completed turn is continued.
- `learn`: automatic training after labels. Explicit `xcb reflex train` works
  regardless.

Settle ships in `observe` because continuing a turn the worker called finished
is a behavior change; once `xcb reflex status settle` shows holdout evidence
you trust, set it to `active`.

## Safety contract

- A reflex refines a decision already inside the safety envelope; it never
  widens it. Route decisions only choose between admitted, eligible routes, and
  the substantial-prompt quality floor cannot be demoted. Settle continuation
  still requires a joined, settled, idle worker with no pending attention,
  failure or uncertain effect, a non-repeating response and remaining
  attempt/time budget. A configured judge keeps its veto.
- Any reflex failure (unreadable ledger, rejected program, runtime error)
  falls back to the pre-reflex behavior. Learning never fails a task.
- The ledger `<state>/reflex.sqlite` stores numeric features, decisions,
  receipts digests and labels, never prompt or response text. It keeps at most
  20,000 observations per reflex, discarding unlabeled ones first, and 256
  generations.
- Labels are one operator's preferences. A learned route predicts which tier
  you would pick, not measured model quality.

## Where it lives

| Piece | Location |
| --- | --- |
| Features, heads, fitting, promotion rule | `crates/xcb-core/src/reflex.rs` |
| Programs | `crates/xcb-runtime/reflexes/*.algal.json` |
| Ledger, runner, admission, training | `crates/xcb-runtime/src/reflex.rs` |
| Route integration | `routing.rs` (`route_reflex`) and `task_classifier.rs` |
| Settle, continuation and implicit labels | `managed.rs` (`settle_decision`, `task_should_continue`, intake) |
| CLI | `xcb reflex` |

Adding a reflex means adding a `Reflex` variant, its heads and prior, a feature
function, a program, and the call site that observes and labels it. The
ledger, fitting, promotion, rollback, CLI and replacement flow are shared.
