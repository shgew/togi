# Candidate edges start a new session in confirmation

A breaking update ends a session: it must be archived with `reset --all` before tuning again ([ADR 0009](0009-compatibility-across-updates.md)). The new session then searches every core from its baseline. Searching again costs most of a night: every core's first crash past its edge is repeated, and on a machine whose edges sit near -40 that is a crash reboot per core, several for cores that fail at more than one step. The operator usually knows from the archived session where each edge lies. `start_offsets` shortens the easy steps down from the baseline, but search still steps deeper from there until it fails.

## Decision

`candidate_edges.<core>` names an offset at which that core starts the new session in confirmation instead of search. Its first `core.phase` records `to: confirmation`, the offset and the reason `configured candidate edge`, with no pass and no failed mark. Confirmation then runs as it does for an edge found by search: all nine isolated trials must pass before the core is confirmed, and a failure sets the failed mark at that offset and restarts the set one count shallower. A core has either a candidate edge or a start offset, not both. Like `start_offsets`, the value is read only when the core's first phase is recorded; once it is, changing the value has no effect on the session.

The value is the operator's claim, not evidence. Nothing is carried from the archived journal: the new session proves every edge under its own ruleset, and its journal shows where each value came from.

## Considered Options

- **Seed failed marks and passes from the archived journal:** rejected because it copies evidence recorded under another ruleset into a session that cannot re-check it, the same risk that rules out migrating journals.
- **Only `start_offsets`:** rejected because search from a start offset still steps deeper until it fails, so it repeats the crashes that located each edge.
- **A command that writes the starting phases:** rejected because the starting offset is configuration recorded in `config.loaded`, like `start_offsets`, and a command would need its own event and replay path for the same fact.

## Consequences

A new session after a breaking update can go straight to confirmation, skipping the search crashes that located each known edge. A candidate edge set too deep costs one failure and a nine-trial restart per count it is off, so the operator should give the failed mark plus one rather than guess deeper. A candidate edge never makes a core deeper than confirmation proves it.
