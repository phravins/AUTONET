# Architecture decision records

Each file here records one decision that shaped the codebase, why it was taken,
and what was rejected to take it. They exist for the question that arrives a year
later — *why is it done this way, and did anyone consider the obvious
alternative?* — which the code itself cannot answer, because code only shows the
option that won.

A decision earns a record when it closes a fork that would otherwise be
re-litigated: something that constrains a milestone's design, contradicts an
apparently reasonable alternative, or accepts a cost on purpose. Ordinary
implementation choices do not need one.

## The rules

**Numbered in order, never reused.** `0001-`, `0002-`, and so on, followed by a
short slug.

**Immutable once decided.** An accepted record is not edited to reflect a change
of mind. A later decision that reverses it gets its own number and marks the
earlier one `Superseded by NNNN`; both stay in the tree. Editing an old record in
place destroys the rejected alternatives and the reasoning around them, which is
the specific thing this directory exists to keep. Corrections to typos and broken
links are fine — corrections to the *decision* are not.

**Status is one of:**

| Status | Meaning |
|---|---|
| `Proposed` | Written and argued, not yet agreed. May still change. |
| `Accepted` | Agreed and in force. Immutable from here. |
| `Superseded by NNNN` | Reversed by a later record, kept for its reasoning. |

**Each record carries its own Consequences section**, including the bad ones. A
record listing only advantages is a record whose costs were not examined.

## Records

| # | Title | Status |
|---|---|---|
| [0001](0001-network-change-during-autonet-run.md) | What happens when the network changes mid-run | Accepted |
