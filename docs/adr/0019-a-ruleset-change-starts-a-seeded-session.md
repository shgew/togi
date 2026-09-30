The candidate-edge checking phase and the same-ruleset BIOS dead end are superseded by [ADR 0020](0020-hunt-and-refine.md).

# A ruleset change starts a seeded session

Every change to `tuner.Ruleset` ended the session: the new build refused the journal ([ADR 0009](0009-compatibility-across-updates.md)), and the operator noted each core's failed mark and confirmed offset, wrote them into `candidate_edges`, ran `reset --all`, and removed the values again once every core had a phase ([ADR 0013](0013-candidate-edges-for-a-new-session.md)). Because only candidate edges carried over, the new session repeated about 12.8 hours of confirmation on the target machine and found every failure again: in its third session core 13 failed resident trials at −49, −48, −47, −45 and −43, each count costing about two hours of rotations.

Storage was not what made the change expensive. `togi status` replays the third session's journal (7,083 events, 21.2 hours) in 25 ms, and the fact events (`trial.intent`, `trial.end`, `failure`) have the same fields in every schema that shipped. What blocked reuse was the refusal, and the tuner copying each core's offset, pass and failed mark from `core.phase` and `tuner.decision` rather than deriving them. What a new session needs from the old one is each core's edge and its attributed failed mark, both facts that hold under any ruleset.

## Decision

- A journal whose ruleset or schema is older than the build's, and newer in neither, is no longer refused. `run`, also in a tuning boot, archives it without writing to it and starts a new session seeded from it. A journal whose ruleset or schema is newer is still refused, and `reset --all` still starts over with nothing carried.
- The new session records `session.carried` once, before any core's first `core.phase`: the sources read and, per core, a candidate edge (its deepest isolated pass) and a carried failed mark (its shallowest failure attributed to it at a known offset, CO 0 included), each naming the session and `seq` it came from. Unattributed failures, failures behind a decision a known defect matches, and values recorded before a `reset --core` of their core are not carried. An isolated trial left in flight at a nonzero offset when the old journal ends counts as a crash at that offset.
- A carried candidate edge starts the core in confirmation, as a configured one does. A configured `candidate_edges` or `start_offsets` value wins over it, but a carried mark clamps any start to one count shallower, and a mark at 0 dead-ends the core until `reset --core`.
- A changed BIOS context carries edges only: a BIOS change can move an edge either way, and failed marks are permanent within a session.
- Transitions chain: a session's `session.carried` is an event in its journal, so the next transition reads it and a later `reset --core` in that session clears it. A first transition whose source carried nothing also reads older archives recorded under the same BIOS context, for as long as each has a different ruleset from the source after it, which reaches the sessions archived only because of a ruleset change.
- Passes and resident offsets are not carried. A new ruleset may accept passes differently, and a resident offset reflects suspect backoffs, which are decisions on unattributed evidence.

This supersedes the refusal in ADR 0009 for older journals and ADR 0013's rule that nothing is carried from the archived journal. [ADR 0003](0003-journal-is-source-of-truth.md) stands: the archived journal is unchanged, and the new journal records every carried value with its source.

## Considered Options

- **A database with migrations (SQLite):** rejected. Replay is already fast; migrations change table shapes, while the need was to reinterpret evidence, which a database does not do by itself; one write and fsync per event already gives crash atomicity; and it would rewrite every user of `internal/journal`, break the certificate's hash over journal lines, and need a second, readable log. Reconsider an append-only events table only if recovery ever needs multi-event transactions that one event cannot hold.
- **Continue the session and re-derive every decision from facts:** rejected. It would also carry passes that a new ruleset might not accept, and it needs events split into observations, commitments and conclusions, identities that span sessions, and a guard against a faulty build reinterpreting history into a deeper profile.
- **Automate the operator's steps only:** rejected because the failures would still be found again.

## Consequences

- A ruleset change no longer costs the search for each known edge or the rediscovery of carried failures, and needs no action from the operator; confirmation still runs under the new rules at each carried edge. `candidate_edges` is only for values the operator wants to impose.
- A carried mark is evidence recorded under another ruleset. It only ever makes a core shallower, and `reset --core` clears it, so a mistaken mark costs depth, never stability.
- The reader for archives must keep accepting every schema that shipped, decoding only the kinds it needs.
- A BIOS change within one ruleset is still a preflight dead end until `reset --all`.
