# Verified folded-state snapshots are a cache, never authority

Amends [ADR 0003](0003-journal-is-source-of-truth.md). The append-only journal remains the source of truth; `state.json` may cache the complete folded state, not just its presentation.

## Decision

Persist the run's recovery fold, shared journal facts, tuner incremental state and defect index as readable JSON. Bind the snapshot to a build identity, its last event sequence and the hash of that journal prefix, and check its contents for corruption before using it. Readers restore verified state and fold only the remaining events. A missing or rejected cache falls back to replay without mutating the state directory.

Hardware runs continue to replay the entire journal on every boot and compare every saved fold with the fresh result. A rejected or differing snapshot is rewritten and recorded with the existing `state.rebuilt` event. This checks caches without allowing them to authorize hardware decisions. Another build rebuilds instead of migrating a snapshot; neither the journal schema nor the ruleset changes.

Keep production's publication after every append and its existing atomic replacement and warning behavior. Simulator and benchmark publication remains deferred until the run finishes. The normative format and rejection rules belong to [the journal specification](../spec/journal.md#state-file).

## Alternatives

- **Replay in every reader:** simple but repeatedly performs the same reduction, including for each dashboard forecast premise.
- **Make mutable state authoritative:** loses the crash-safety and causal history of the journal.
- **Migrate snapshots between builds:** adds a second versioned data format when replay already supplies the migration.
- **Trust a snapshot in hardware runs:** lets a cache affect safety decisions without the independent full replay.
