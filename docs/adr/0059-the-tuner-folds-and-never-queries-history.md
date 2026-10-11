# The tuner folds the journal and never queries its history

Status: see [the index](README.md).

Design of [#614](https://github.com/shgew/togi/issues/614), carried into the native Rust tuner ([#638](https://github.com/shgew/togi/issues/638), [ADR 0058](0058-native-rust-rewrite.md)).

## Context

The Go tuner is a fold of journal events that answers "what next" by reading what it folded. Two things grew from that. Queries consulted history: a pass count scanned every ledger entry of a class, a rerun was retired inside `Next`, a projection rebuilt a view from the whole journal, and `Forecast` replayed the journal once per premise. Then [#604](https://github.com/shgew/togi/issues/604) made the queries fast with caches keyed by invalidation: `gen`, `checkingEpoch`, `evidenceEpoch`, `limitEpoch`, `phasesEpoch`, `cycleMemo`, `roundMemo`, `derivedBySeq` and their kin. Each cache was correct only while every event that could change its inputs bumped its epoch, so a rule and its invalidation lived in different files, `memo_check_test.go` existed to catch the epochs that were missed, and a missed bump showed as a stale answer after a resume.

A native tuner can keep the cost without the epochs by moving the work to the one place that sees each event once.

## Decision

**One `State`, one exhaustive `fold`, read-only queries.** `State` is `Clone + PartialEq + Debug`, a plain struct with one substate per rule group (topology, cores, search, constraints, the phases lifecycle, evidence, flight, retry, pending warning, failures, and the groups that follow). `State::fold(&mut self, &Event) -> Result<(), FoldError>` is one `match` on the journal's payload enum, an arm per kind with no catch-all, so a new journal kind does not compile until the tuner says what it does with it. Everything a rule needs from an event is computed in the arm of the events that can change it: the requirement a trial was scheduled for before its intent folds, the pass counts a trial end changes, the rerun an event retires, the phase an event ends. No query writes, and no event is followed by a pass that recomputes whatever might have changed.

**Queries read the state and never the events.** The decision functions take `&self` and no event list, so they cannot walk history. Repeating one returns an equal answer and leaves the state `==`. A projection or a forecast is a read of the folded state; a forecast clones the state per premise and folds synthetic events into the clone, instead of replaying the journal.

**Cost is bounded by distinct evidence, not by events.** Evidence lives in cells keyed by class, backend package and profile; a cell holds ascending seqs, and a count is a binary search per cell that covers the profile. Ten thousand passes at one profile are one cell. The test build counts the cells a fold or a query visits, and a test shows the visits do not grow with the passes folded.

**Validation is part of the fold.** An event that contradicts the state, such as a second `session.start`, a core outside the topology, a profile that does not cover the topology, an intent that belongs to no open scheduling context, a trial end for no open trial or an event that does not follow the last, is a `FoldError`. Each arm validates what its payload names before it changes anything, but an error still poisons the state: every later `fold` fails with `FoldError::Poisoned` and the decision functions answer nothing, because a state that may have part-applied an event no longer stands for the journal. A session treats the error as a corrupt journal. Disagreement with a recorded decision is not an error, because a resumed journal replays what it recorded.

**Resume is a fold.** A fresh fold of any prefix of the journal equals the state folded incrementally to that prefix, which the tests check at every prefix of generated journals.

## Considered Options

- **Query the whole history on demand:** rejected. It is the Go tuner before #604: simple, and linear in the journal for every decision, count and projection, so a long session slows down in proportion to its length and a forecast pays that cost per premise.
- **Keep #604's caches keyed by invalidation:** rejected. They are fast and correct only while every invalidation is remembered; the rule and the epoch it must bump are in different places, and the failure is a stale answer that shows up only after a resume. Doing the work in the arm that changes the input removes the cache and the invalidation together.
- **A database or an on-disk index of the journal:** rejected. The tuner is a pure synchronous core ([ADR 0058](0058-native-rust-rewrite.md)); an index adds I/O, a second source of truth and a way for the state to disagree with the journal, to speed up a fold that is already linear in the events and constant per event.

## Consequences

- A rule lands with its fold arm and its decision function, and is tested through a state built by folding events.
- State grows with distinct evidence, failures and combinations, not with trials. History that remains is kept on purpose and bounded by those.
- Adding a journal event kind is a compile error in `fold` until its arm exists; an event that changes nothing for the tuner says so in an explicit arm.
- `Clone` makes forecasts and the warm simulator's retained state cheap, and `PartialEq` replaces comparing two tuners by their answers.
