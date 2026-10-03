A coding agent sometimes stops with a task unfinished and waits for a short reply before continuing. Repeating that reply is easy to automate; deciding when it is appropriate is harder. xcb uses small learned rules, called reflexes, to recognize those turns from your own replies.

[ALGAL](https://algal.computer) provides the runtime for those rules. It lets xcb keep the calculation separate from the learned values and replay a past decision from the inputs it recorded.

## Keep the calculation fixed while the values learn

A reflex takes numeric features from a turn, such as whether the report appears unfinished or asks for approval. It combines those features with learned parameters to produce a decision:

```text
decision = program(features, parameters, evidence)
```

The ALGAL program performs that calculation without model calls or external actions. Learning creates a new set of parameter values; it does not edit the program. xcb records the program fingerprint and parameter version with each observation, so the same recorded inputs can reproduce the same answer.

That is useful when inspecting a bad decision. You can distinguish a change in the learned values from a change in the program, and replay the calculation without asking a model to reconstruct what happened.

## Learn from the replies you give

The continuation and confirmation rules default to `auto`. They initially watch your turns and replies. A request to continue labels one kind of unfinished turn; an approval or refusal supplies evidence about a request for permission.

Before an automatic rule can act, xcb checks it against your labeled history in time order. Each turn is scored by a version that had not yet learned from that turn. Decisions xcb answered itself can contribute to training, but cannot establish that it is ready to act automatically. Otherwise it would be grading its own answers.

Once a rule meets the required threshold, xcb still leaves about one qualifying turn in ten for you. Those fresh replies let it monitor whether the rule continues to match your choices and return it to watching when measured performance falls. This evidence can lag a change in your preferences; the [reflexes reference](/docs/reflexes) gives the thresholds and reassessment rules.

You can keep a rule in `observe`, where it never acts, or choose `active`, which bypasses the history requirement. The separate route reflex starts active and chooses a model tier among accounts and models already able to take the task. It predicts your preferred tier, rather than measuring which model will produce the best work.

## Keep permission decisions outside the learned program

xcb applies its own checks before a confirmation rule can answer. Detected cues for deployment, deletion, payments, access changes, messages to people, or a handoff to you block automatic confirmation. A configured judge can also veto a remaining request.

Replacing a reflex program cannot remove those checks. xcb accepts replacement programs only when they have no external actions or model calls. The learned calculation supplies a decision inside the host's existing rules.

## Inspect a decision and change the mode

The status command shows the continuation rule and the evidence behind it:

```sh
xcb reflex status settle
```

You can evaluate your own history without storing it, or restore the shipped parameters:

```sh
xcb reflex import settle history.jsonl --dry-run
xcb reflex rollback settle 0
```

The local reflex ledger stores numeric features, decisions, labels, and fingerprints, without prompt or response text. The managed harness remains experimental. ALGAL also records its task transitions, which [xcb can replay offline](/blog/replayable-task-history) to check the consistency of a saved task history.
