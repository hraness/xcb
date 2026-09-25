A coding agent finishes a turn with "Pushed; waiting on CI." Another ends with "Should I merge it?" You answer the first with "continue" and the second with "yes, go ahead", and by the fortieth time that day you would happily let something answer for you. The catch is the forty-first turn, where "go ahead" would have deleted a branch or shipped to production. xcb handles these small, repeated calls with reflexes, and it runs each reflex as an ALGAL program so the reflex has to prove itself on your own history before it acts.

## Fewer interruptions, and no guesses on risky turns

Someone who routes work through xcb wants fewer interruptions. A turn that clearly stopped short should keep going without a typed "continue". A request for a harmless next step should get its "yes". A new task that needs a frontier model should get one, and a quick question should not.

They also want limits on that help. A shortcut learned last week should stay out of risky requests, act only where your history shows it is right, and leave a record anyone can check afterward. xcb keeps the part that learns small and runs it where every decision can be recomputed.

## What ALGAL does

[ALGAL](https://algal.computer) is a language and application VM for bounded agent programs. An ALGAL program, which ALGAL calls an organism, is written as data: the steps, how they connect, and the limits each run must respect, such as how many steps it may take and how many model calls it may make. The host application decides what the program is allowed to touch. When a program runs, it leaves a record of what it did, so someone can explain the result later without calling a model again.

xcb embeds the Rust ALGAL runtime directly, pinned to one exact commit of the ALGAL repository, so an ALGAL update never changes xcb's behavior until xcb itself moves the pin.

## How xcb runs its reflexes on ALGAL

xcb ships two reflexes. The route reflex picks a frontier or standard model tier for a new task. The settle reflex reads how a worker's turn ended and has two heads: one decides whether the turn stopped short and should continue, and one decides whether the turn is asking for a go-ahead that xcb may answer.

Every reflex decision has the same shape:

```
decision = program(features, params, evidence)
```

The program is an ALGAL organism that does arithmetic and nothing else. It has no model calls and no side effects, and its declared limits say so. A simplified sketch of the shipped settle program looks like this:

```json
{
  "contract": "algal.organism.v1",
  "budgets": { "maxAgentCalls": 0, "maxSteps": 6 },
  "cells": [
    { "id": "src", "kind": "input" },
    { "id": "head", "kind": "expr" },
    { "id": "decide", "kind": "expr" }
  ]
}
```

The features are numbers xcb computes from the end of the worker's report: how many tool calls the turn made, whether the last paragraph reads as still in progress, whether it asks a question, whether it names a risky step. The params are one small scoring model per head, with a threshold. Learning adds a new version of those params. It never edits the program.

That split gives xcb two rules:

1. **Same inputs, same answer.** The same program, params, features, and evidence always give the same decision and the same run fingerprint. xcb records the program's fingerprint and the params version with every observation, so any past decision can be run again and must come out identical.
2. **The program cannot widen its own permissions.** You can replace a reflex program with your own, but xcb accepts it only if it has no effectful steps and no model calls. The veto on risky requests lives in xcb itself, not in the replaceable program, so a custom program cannot remove it.

### When a head may act

The settle heads default to a mode xcb calls `auto`:

```json
{ "route": "active", "settle": "auto", "confirm": "auto", "learn": true }
```

In `auto`, a head starts out only watching. Your replies label its decisions: "continue" or "push it" after a finished turn marks it as stopped short, "yes" or "lgtm" marks a turn that asked for approval, "wait" or "no, ..." marks a miss.

After training passes and imports, xcb replays your labeled turns from the shipped starting point, so each turn is scored by a version of the head that had not yet learned from it. Only your own labels count toward this. Labels from turns xcb answered itself train the head but cannot certify it, because they would only confirm its own choices. On the newest 1,500 of your labeled turns (for the confirm head, only requests it could answer, with no risk or hand-off cue), a head is certified at the lowest threshold where it fired at least 30 times and a conservative 99% lower bound on its precision reaches a floor: 0.75 for continuing a stopped turn, and 0.85 for answering "yes", since saying yes on your behalf is the bigger step.

Once certified, a head acts only on turns it scores at or above that threshold. About one such turn in ten, chosen by a hash of the task and turn, is still left for you. Those replies are the only unbiased evidence a head keeps getting after it starts acting, and they are what can take its certificate away. If measured precision falls more than 0.05 below the floor, the head goes back to watching. You can also set a head to `observe` so it never acts, or to `active` so it acts without a certificate.

Even a certified confirm head never says "yes" when the turn hands something to you, when its report mentions deletion, secrets, production, or spending, or when the request proposes a deploy, release, tag, migration, removal, payment, access change, or message to people. A configured judge can still veto what is left.

The route reflex works differently. It is `active` by default, and its starting version reproduces how xcb chose tiers before reflexes existed. It only ever chooses between routes that were already eligible, and a long prompt's minimum quality tier cannot be lowered.

## What you get

Fewer "continue" and "yes" messages on the turns where your own history says xcb gets them right, and none on the turns where it has not shown that yet. The reflexes reference reports a check on one operator's private history of 2,428 follow-up messages: the continuation head cleared its floor on the largest time-ordered split, and the confirm head never fired often enough to reach its 0.85 floor. On that history, `auto` would continue stopped-short turns once enough fresh replies accumulate, and leave every go-ahead request to that person.

You can inspect what each head did. `xcb reflex status settle` shows each certificate and the evidence behind it. `xcb reflex import settle history.jsonl --dry-run` replays a history you supply and shows whether it alone would certify each head, without storing anything. `xcb reflex rollback settle 0` puts the shipped starting version back. The local ledger keeps numeric features, decisions, labels, and fingerprints, and never the text of a prompt or response.

The same runtime also records managed task transitions and runs the small resumable controllers behind scheduled work, which is what lets `xcb tasks verify` replay the chain of records a task left on your machine and check it against the stored record. [Replayable task history](/blog/replayable-task-history) covers that side.

## Limits

xcb's README calls the managed harness experimental, and the reflexes are part of it. A reflex learns one person's preferences and does not measure model quality, so a learned route predicts the tier you would pick. The 0.75 and 0.85 floors hold only approximately: up to seven thresholds are tried and the test repeats as labels arrive, so the real chance of certifying a head below its floor is somewhat higher than 1%, and once a head acts, a real decline shows up only after a few hundred of the replies left to you. Replaying a decision confirms that xcb's recorded inputs produce the recorded answer. It does not confirm that the answer was the right call for your work.

For the rest of the product, start with [Introducing xcb](/blog/introducing-xcb). The full rules, including every reply category and the continuation checks, are in the [reflexes reference](/docs/reflexes).
