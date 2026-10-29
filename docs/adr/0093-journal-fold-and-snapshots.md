# The journal is authoritative; folds drive decisions and snapshots cache reads

Status: see [the index](README.md).

## Context

A tuning session needs a readable causal trail of actions, observations and decisions, including across crashes. An append-only JSONL journal gives the real machine and simulator the same replay model without maintaining a database as another representation of history.

Replaying once is different from querying history for every decision. Repeated history scans make a long session progressively slower and multiply the cost of forecasts. Invalidation-keyed caches avoid scans only while every event remembers to invalidate every dependent answer. Moving the reduction into the event's fold arm keeps the rule and the work that updates its inputs together.

Read-only commands repeatedly need the same folded state. A complete verified snapshot can avoid repeating that reduction without changing which record is authoritative. Hardware decisions need an independent replay rather than trust in a persisted cache.

History: [0003](0003-journal-is-source-of-truth.md), [0059](0059-the-tuner-folds-and-never-queries-history.md) and [0077](0077-verified-folded-state-snapshots.md).

## Decision

### Journal authority

**The journal is the source of truth; there is no database.** State is derived by replay, so every decision remains traceable in plain text and crash recovery uses the same evidence as normal execution. The [journal rules](../spec/journal.md#rules) own durability, append ordering, repair and locking; the [event format](../spec/journal.md#event-format) owns its representation.

### Exhaustive folded state

**The tuner has one `State`, one exhaustive event fold and read-only queries.** The fold matches the journal payload enum without a catch-all. A new event kind requires an explicit arm, even if it changes nothing for the tuner. Each arm computes the derived inputs its event changes; there is no later recomputation pass and no separate invalidation epoch to remember.

Decision functions take only the folded state, never an event list, and cannot mutate it. Projections read that state. Forecasts clone it per premise and fold synthetic events into the clone instead of replaying history. State is cloneable, comparable and debuggable, so equal-prefix replay and incremental folding can be compared directly.

**Work is bounded by distinct evidence, not repeated events.** Grouping evidence by the dimensions a rule distinguishes avoids revisiting every repeated pass for a count or decision. Retained sequence histories may grow, but a query does not scan all trials; the [tuner specification](../spec/tuner.md#evidence) owns the current evidence keys and provenance. No obsolete per-core phases, hunts or combinations are implied by this architecture.

Validation belongs in the fold. A contradictory event makes the fold fail and poisons the state, because a partially applied event cannot be trusted to represent the journal; later folds and decisions cannot continue from it. Replay follows recorded decisions rather than treating a difference from today's proposed decision as corruption. The [journal specification](../spec/journal.md#state-file) owns invalid-journal handling, and [tuner invariants](../spec/tuner.md#invariants) own decision reproducibility.

### Verified read cache

**`state.json` caches the complete folded state, never authority.** It includes the recovery fold, shared journal facts, tuner incremental state, defect index and the presentation needed by readers, not merely a dashboard projection. Verification binds it to the producing build, the last event sequence and exact journal prefix, and checks its contents for corruption. A read-only reader restores verified folds and processes the tail; a missing or rejected snapshot falls back to replay without changing files.

**Hardware runs replay the full journal on every boot and compare saved folds with the fresh result.** A rejected or differing cache is rebuilt rather than allowed to authorize a hardware decision. A different build rebuilds from the journal rather than migrating the snapshot. The [state-file specification](../spec/journal.md#state-file) owns verification metadata, rejection, publication, retained presentation and warning behavior; those details are not a second format contract here.

## Considered Options

- **SQLite:** rejected. Transactions are useful, but a database is opaque to plain reading and creates a second representation of the same history.
- **A mutable state file without history, or snapshots as authority:** rejected. Either loses the causal trail and journal-based crash recovery.
- **Query the whole history on demand:** rejected. Decisions, counts and projections become linear in session length, and forecasts pay again per premise.
- **Caches keyed by invalidation epochs:** rejected. A missed bump silently leaves stale answers, with the rule and its invalidation in different places; fold-time maintenance removes both the cache and that obligation.
- **A database or on-disk journal index for tuner queries:** rejected. It adds I/O and a way for state to disagree with the journal to a pure synchronous core whose replay is already a linear fold.
- **Replay in every reader:** rejected. It repeats the same reduction for readers and forecast premises when verified folds can be reused.
- **Migrate snapshots between builds:** rejected. It creates another versioned data format when journal replay already rebuilds the state.
- **Trust snapshots in hardware runs:** rejected. A cache must not affect safety decisions without independent full replay.

## Consequences

A rule's update and read logic stay together, and an event addition cannot silently bypass the tuner fold. Recovery and incremental execution have the same state model, tested by replay equivalence. Readers can avoid redundant reduction, but the journal remains necessary and sufficient to rebuild the cache; cache loss or rejection is not evidence loss. Snapshot verification and full hardware replay cost work deliberately, rather than trading away the authoritative history or independent safety check.
