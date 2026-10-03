# Coordination recipe diagnostic

This optional fixture reproduces a source-inspection and synthesis run about
xcb's ALGAL controller suspension. It completed offline replay, but its answers
contain citation errors. It is available for evaluation and has not been chosen
as a default recipe or for answer quality.

With a build that provides `xcb context`, run from the repository root:

```sh
xcb context replay examples/context-recipes/coordination/recipe.json --results examples/context-recipes/coordination/results.json
```

The command uses the saved summaries and makes no provider calls. Compare its
recipe digest, receipt digest and summary with [replay.json](replay.json).
[protocol.json](protocol.json) records the source hashes, original assessment
criteria, attempt limits and usage. The three execution files preserve their
original bytes. Their source text comes from four public files at
[`hraness/xcb@54a8ea4`](https://github.com/hraness/xcb/tree/54a8ea46b6655236afbf307df71c9a1cb7610c02).

## Observed usage

One baseline and three recipe attempts used six xcb route invocations on the
same model and account. An invocation can contain several model/tool exchanges;
the number of model API round trips is unknown.

| Condition | Routes | Input tokens | Cache read | Cache write | Output tokens | Tools | Route seconds |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Baseline | 1 | 3900 | 31386 | 10658 | 1874 | 9 | 26.5488 |
| Stopped attempts combined | 3 | 30 | 0 | 15093 | 2459 | 0 | 40.1721 |
| Completed recipe | 2 | 20 | 0 | 9243 | 1763 | 0 | 23.4222 |
| Entire diagnostic | 6 | 3950 | 31386 | 34994 | 6096 | 9 | 90.1430 |

These are reported token categories, with reasoning already included in output.
API-dollar cost is unavailable for this subscription route. Route time excludes
preparation, offline work, repairs and time between attempts. The baseline used
five directory listings, two searches and two reads. Recipe calls received
excerpts and used no workspace tools.

The first recipe stopped after one 822-byte report exceeded a 768-byte limit.
The second produced 980-byte and 1578-byte reports, then stopped at its 1024-byte
limit before synthesis. The completed recipe used one combined inspection with
a 6 KiB limit on its JSON representation, followed by synthesis. Its reports
were 1514 and 1963 UTF-8 bytes. Both stopped attempts are included above. Changed
budgets, changed inspection granularity, fixed ordering and one question limit
the comparison to this adaptive diagnostic.

## Answer and citation limits

Both answers covered the first three assessment concepts: resource release,
recording the pending call and child identity, and consuming the linked child's
conclusive result. Both only partially covered the fourth: evidence required
after uncertain execution. The supplied files describe joined outcomes and
unsettled runs but do not explicitly state the criterion's independent
process-exit-witness requirement.

The recipe's inspection cited bytes 1–500 for quotations located at 824–890 and
1288–1358. It attributed the exact-child-result quotation at 1361–1434 to the
second chunk, which starts at 1536. Synthesis kept abbreviated hashes and dropped
the byte ranges. The saved answers preserve these errors so future citation
checks can detect them. Successful replay alone does not validate an answer.

The fixture contains public source snapshots, model-written summaries and
aggregate observations. Account/session identifiers, route responses and raw
provider records are excluded. Answer authorship and the independent AI
assessment are recorded in the protocol; no human review is claimed.
