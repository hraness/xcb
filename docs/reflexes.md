# Reflexes: learned routing and turn categorization

A reflex is a small decision the harness makes many times a day, and that it
can learn from how you respond to it. xcb ships two:

| Reflex | Decides | Heads | Default |
| --- | --- | --- | --- |
| `route` | Frontier or standard model tier for a new task | `judged` (with judge evidence), `plain` (prompt shape and keyword cues only) | `active` |
| `settle` | How a settled worker turn ended: `done`, `stopped_short`, `confirm`, `question`, `needs_action`, `needs_approval`, `blocked`, `interrupted`, `uncertain`, ... | `unfinished`, `confirm` | `auto` |

Auto-continuation is the settle reflex acting: a completed turn categorized as
`stopped_short` is continued in its session when the `unfinished` head acts
and every deterministic continuation gate passes. A turn categorized as
`confirm` (the worker proposed a step and asked for the go-ahead) is answered
"yes" only when the `confirm` head acts and the request names nothing risky.
By default both heads are `auto`: they act only after your own replies
certify their precision (see [Auto](#auto-acting-once-certified)), and they
stop acting if it falls.

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
- `settle.unfinished` and `settle.confirm` are generic priors fitted on
  2,428 real operator follow-ups (see [Measured](#measured-on-operator-history)).
  Their features read the end of the worker's report: how much work the turn
  did (tool calls, the strongest single signal), whether the last paragraph
  is in progress ("Pushed; waiting on CI"), asks for a go-ahead ("Should I
  merge it?"), hands something to the user ("you'll need to sign in"), names a
  risky step (delete, drop, deploy, publish, spend, credentials), or reads as
  a structured final report. Both are tuned for precision: at the shipped
  thresholds `unfinished` is right four times in five and `confirm` two times
  in three, and they catch about 30% and 20% of the real cases.

Route's generation 0 is behavior-identical to xcb before reflexes. Settle
ships in `auto`, so its fitted prior categorizes and learns from the first
turn but acts only once your replies certify it.

## How it learns

Decisions are observed; your behavior labels them. What you say after a task
completes is categorized, and each category labels the settle heads:

| Your reply after a completed task | Category | `unfinished` | `confirm` |
| --- | --- | --- | --- |
| `continue`, `keep going`, `go on`, `proceed`, `go` | continue | yes (1.0) | no (0.5) |
| `push it`, `open a PR`, `merge it`, `ship it` | deliver | yes (1.0) | no (0.5) |
| `yes`, `go ahead`, `do it`, `sounds good`, `lgtm` | approve | no (0.5) | yes (1.0) |
| `done`, `I signed in`, `I added the key` | handoff done | no (0.5) | no (0.5) |
| `no, ...`, `wait`, `that's wrong`, `why did ...` | correction | no (0.5) | no (0.5) |
| anything else | other (a new request) | no (0.5) | no (0.5) |
| `status`, `did you merge?` / `limits are back` | status / quota | — | — |

Other signals:

| Signal | Reflex | Label | Weight |
| --- | --- | --- | --- |
| A continuation the settle reflex started did real work (made tool calls) | settle, the head that acted | yes | 0.5 |
| That continuation made no tool call | settle, the head that acted | no | 0.5 |
| You cancel a continuation the settle reflex started | settle, the head that acted | no | 0.75 |
| You ask for a stronger model ("use opus", "better model") after a task | route | frontier | 1.0 |
| You ask for a lighter model ("use sonnet", "cheaper model") | route | standard | 1.0 |
| `xcb reflex label <reflex> <task> <label>` | either | explicit | 1.0 |

The continuation labels keep an active reflex learning after you stop having
to type "continue". With settle `active` or `auto`, a `continue` reply (or a
short "yes" to a turn categorized `confirm`) reopens the completed task in its
session instead of starting a new task; in `observe` it only labels. A label
is replaced only by a heavier one, so an inferred label never overwrites an
explicit one.

### Forward trials

After every 16 labels, and whenever you run `xcb reflex train`, each head
takes one learning step:

1. If the head has no open trial, xcb fits a **challenger**: the MAP fit of
   the shipped prior given the newest 4,096 labels (logistic regression with
   a Gaussian prior centered on generation 0), so a few labels cannot move a
   head far and repeated passes converge instead of drifting.
2. The challenger and the active head are then both scored on the labels
   that arrive **after** the challenger was fitted, which neither was fitted
   on.
3. Once the trial has 48 such labels, the challenger is promoted if on them
   it lowers log loss by at least 0.002 without losing more than 0.02
   accuracy or 0.02 AUC. Either way the trial closes and the next pass fits a
   new challenger.

Promotion appends a generation that records its parent, how many labels were
available when it was fitted, and the trial that justified it.
`xcb reflex rollback <reflex> <version>` reactivates any recorded generation,
and version 0 restores the prior.

Why forward trials rather than a fixed holdout: a hashed one-in-five holdout
withholds a fifth of every operator's evidence from training forever, and it
measures a head on the same period it was fitted on, which hides drift. In a
replay of real operator history started from a weak prior, forward trials
reached a prequential AUC of 0.77 (precision 0.79 at 0.6), against 0.75 for
the hashed holdout, 0.70 for a newest-window holdout and 0.58 for not
learning at all. Starting from a good prior, learning neither helped nor
hurt much (0.77 against 0.79), which is the promotion gate doing its job.

`xcb reflex status` reports each head's **live** metrics (accuracy,
precision and recall at its threshold, and AUC on labels that arrived after
it became active), the open trial's progress and its certificate.

### Auto: acting once certified

`auto`, the default for settle and confirm, observes until your own replies
show that a head is precise enough to act for you, then acts, and goes back
to observing if it gets worse. After every training pass and every import:

1. The head's retained labels are replayed from the shipped prior through the
   same learning loop, so every turn is scored by a head that had not yet
   learned from it.
2. Only **operator** labels count: your replies, explicit labels and imported
   history. Labels xcb earns by acting (continuation outcomes and
   cancellations) still train the head, but they cannot certify it, because
   they would confirm its own choices.
3. On the newest 1,500 such turns (for `confirm`, only requests it could
   answer: no risk or hand-off cue), the head is certified at the lowest
   threshold, from its own upward in steps of 0.05, at which it fired on at
   least 30 turns and the one-sided 99% lower bound on its precision reaches
   the floor: **0.75** for `unfinished` and **0.85** for `confirm`, since
   answering "yes" for you is the bigger step.
4. A certified head keeps its certificate while its precision stays within
   0.05 of the floor. Below that it goes back to observing.

While certified, a head acts only on turns that the active generation scores
at or above the certified threshold. The runtime computes that probability
itself, so a custom program cannot raise it. About one such turn in ten,
chosen by a hash of the task and turn, is still left for you. Your replies to
those turns are the only unbiased evidence a head keeps getting once it acts,
and they are what can withdraw its certificate.

`xcb reflex status settle` shows each certificate and its evidence.
`xcb reflex import settle <file> --dry-run` shows whether that history alone
would certify each head. If the history is the one a prior was fitted on,
certification on it is optimistic; see the measurements below.

## Measured on operator history

A private corpus of 2,428 follow-up messages from Claude Code, Codex and
Devin sessions, each paired with the assistant turn before it and its tool
call count. Only aggregate numbers are published.

| | |
| --- | --- |
| Follow-ups that were "continue" or a delivery request | 30% |
| Follow-ups that approved a proposed step | 7.5% |
| Follow-ups that said a handoff was done | 1.7% (no head: too rare to fire usefully) |
| "continue" after a turn with no tool calls | 15% |
| "continue" after a turn with 40+ tool calls | 45% |
| `unfinished` cross-validated AUC, text features only / plus tool calls | 0.71 / 0.79 |

The shipped priors on the same history, at their thresholds:

| Head | Threshold | Precision | Recall | AUC |
| --- | --- | --- | --- | --- |
| `settle.unfinished` | 0.65 | 0.80 | 0.30 | 0.82 |
| `settle.confirm` | 0.45 | 0.66 | 0.21 | 0.84 |

These are in-sample for the fit, so expect a little less on your own history.

Out of sample, the `unfinished` head was fitted on the oldest 50%, 60% or
70% of the history in time order and scored on the rest. It reached precision 0.77
to 0.87 at thresholds 0.65 to 0.75. On the largest split, the certificate's
lower bound cleared 0.75; on the two smaller ones it fell short (0.66 to 0.73).
`confirm` never fired on 30 requests it could answer. So on this history `auto`
continues stopped-short turns once enough fresh replies accumulate, and it
leaves go-ahead requests to the operator until the head earns its 0.85 floor.
The earlier keyword-only settle prior missed most of these cases: most
"continue" replies followed turns that did a lot of work and ended in progress
rather than turns that promised a next step, and most approvals followed a
direct question the old classifier did not separate from done.

For route, on one operator's 320 first prompts the keyword fallback carried
no signal for the tier they picked (AUC 0.44); a learned plain head recovered
part of it from prompt shape alone (0.65) without a judge call.

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

Custom programs must compile with exactly those JSON inputs and one JSON
`decision` output. Files are limited to 64 KiB and must be regular files, not
symlinks. The runtime permits at most 16 cells, 64 edges, 64 steps, 100,000 work
units, 64 KiB of context, 16 KiB of output and depth 1. Evaluation runs off the
supervisor thread, joins its bounded pure work, and rejects results returned
after five seconds. Oversized or invalid decisions fall back to ordinary routing
or settlement.

## Commands

```
xcb reflex                      # status of both reflexes
xcb reflex status route         # generation, program digest, live metrics, open trials
xcb reflex train settle         # decide finished trials, fit new challengers
xcb reflex label route t_… frontier
xcb reflex label settle t_… unfinished   # or confirm, or done
xcb reflex rollback route 0     # back to the shipped prior
xcb reflex import settle history.jsonl --dry-run
xcb reflex check my-route.algal.json
```

`import` bootstraps from your own history, oldest line first. Each line is
`{"id", "text", "label"[, "weight"]}` plus, for route, an optional `judge`
(`difficulty`, `scope`, `ambiguity`, `stakes` as criterion indices 0–4 and
`frontier` 0–1) and, for settle, an optional `head` (`unfinished` or
`confirm`, default `unfinished`) and `tool_calls`. The examples are replayed
in order as forward trials from the shipped prior, and the report shows the
prequential metrics: every example scored before it was learned from. Heads
that won a trial in the replay are adopted as a new generation. `--dry-run`
reports without storing anything. Only the derived features are stored.

## Configuration

`config.json` `extensions.reflexes`:

```json
{ "route": "active", "settle": "auto", "confirm": "auto", "learn": true }
```

- `off`: the reflex does not run; routing and continuation behave as before.
- `observe`: decisions are recorded and learned from, but do not act. Settle
  categories are still shown on tasks.
- `active`: the decision acts. For route it sets the tier; for settle a
  `stopped_short` completed turn is continued.
- `auto` (settle and confirm): observe until the head is certified, then act
  as `active` does on turns scored at or above the certified threshold,
  leaving about one in ten to you.
- `confirm`: whether a `confirm` turn is answered "yes, go ahead". It acts
  only when `settle` is not `off`, the `confirm` head acts (`active`, or
  `auto` and certified), the turn completed,
  nothing is handed to the user, the report never mentions deletion, secrets,
  production or spending, the asking paragraph proposes no deploy, release,
  tag, migration, removal, payment, access change or message to people
  (stems match with their inflections), and a configured judge agrees
  that the step stays within the task, is reversible and needs no new
  permissions. A vetoed request can still continue deterministically if its
  turn was interrupted by a limit, with the generic prompt. A judge may veto
  that continuation but can never turn the request into a "yes". The asking
  paragraph is the last one and also the last one with a question, so a
  question followed by a list of options is read in full.
- `learn`: automatic training after labels. Explicit `xcb reflex train` works
  regardless.

Set `observe` to keep a head from ever acting, or `active` to let it act
without a certificate. `off` disables settle entirely, including `confirm`.

## Safety contract

- A reflex refines a decision already inside the safety envelope; it never
  widens it. Route decisions only choose between admitted, eligible routes, and
  the substantial-prompt quality floor cannot be demoted, even by a replaced
  program. Settle continuation
  still requires a joined, settled, idle worker with no pending attention,
  failure or uncertain effect, a non-repeating response and remaining
  attempt/time budget. A configured judge keeps its veto. The confirm risk
  veto is in the runtime, not the replaceable program, so a custom program
  cannot remove it.
- A turn reconciled after a restart has no tool-call count, so settle leaves
  it uncategorized rather than scoring it as a turn that did no work.
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
| Features, heads, fitting, forward trials, replay, reply categories | `crates/xcb-core/src/reflex.rs` |
| Programs | `crates/xcb-runtime/reflexes/*.algal.json` |
| Ledger, runner, admission, training | `crates/xcb-runtime/src/reflex.rs` |
| Route integration | `routing.rs` (`route_reflex`) and `task_classifier.rs` |
| Settle, continuation, confirm veto and implicit labels | `managed.rs` (`settle_decision`, `task_should_continue`, `continuation_outcome`, intake, cancel) |
| CLI | `xcb reflex` |

Adding a reflex means adding a `Reflex` variant, its heads and prior, a feature
function, a program, and the call site that observes and labels it. The
ledger, fitting, promotion, rollback, CLI and replacement flow are shared.
