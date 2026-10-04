# Failover at a usage limit

When a turn in `xcb run` or the terminal stops because the provider reported
a usage limit, xcb moves the same session to another account or model and
continues the conversation from its last confirmed checkpoint. The transcript
stays; the next turn starts with a note that the previous provider hit a
limit and that the provider should check current files before changing them.
`auto_failover` in the config turns this off.

## When xcb switches

xcb switches only after a turn that settled with a usage limit: the
provider's processes have exited, xcb has recorded how the run ended, nothing
is waiting for an answer, and the turn left something to continue from (a
reply, or no file changes at all). A turn whose ending xcb could not confirm
keeps its account and is never retried elsewhere. Failover has no time
budget of its own: a long turn that then hits a limit still moves. It stops
after sixteen routes in one task, or when you cancel.

A provider's permission or safety denial stops the task. xcb does not move
the denied action to another provider. See [provider approval modes](provider-permissions.md)
for the automatic review settings and the decisions that need the owner.

## Which route it picks

The candidates are the routes automatic routing would pick now: accounts
with a supported provider build that are signed in, enabled, idle, and not
at a known usage limit, with a recently seen model. An account without a
usage meter, or whose last reading is more than five minutes old, has no
known limit and is a candidate; a limit is known only from the provider's
own report. The route that hit the limit, every route this task
already ran, and any account that reported an account-wide limit during this
task are left out.

Among those, xcb ranks routes as [quota routing](quota-routing.md) does. The
[preference stack](quota-routing.md#preference-stack) decides first: after an
account limit the next pick is the same stack pattern on another account
(the account that ran a session longest ago), then the tier's next pattern,
so a build-out that started on Astra moves to Astra on another account before
it moves to Fable. Routes at the same stack position are grouped to keep the
subscription rotation close to the work:

1. the same model on another account, so quality is preserved;
2. the same provider's other models;
3. other providers.

Within a group, the router's order holds: task type, relative quality, cost,
and latency, favorites, and remaining usage before a reset. Routes that tie
go to the account that ran a session longest ago, so equal subscriptions
take turns instead of the same one always winning. That tie-break never
outranks a quality tier, a favorite, a pin, or usage pace.

A prompt that opens with `Use Claude` or `Use Codex` pins the
provider for the whole task, including failover. Pins narrow the choice;
xcb never widens past them. The optional judge can reorder the candidates
that already qualify; it cannot add one.

## When nothing can take the task

If no other account can take the task now, xcb stops and says why, naming
the limited account and each reason it passed over the others: at a usage
limit, signed out, disabled, at its run limit, on a provider build
xcb has not checked, outside the pinned provider, or already tried on this
task. The notice ends with the earliest known reset. Reaching a reset lets
xcb try again; it does not prove the provider will accept the turn.

Managed tasks use the same eligibility and order but start a fresh session
on the new route and hand it the previous route's report, because a managed
task owns its own transcript and prompt.
