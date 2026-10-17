# Yield without finishing phase-two movers

Supersedes [ADR 0054](0054-two-phases-replace-the-deepening-loop.md)'s rollback-A finish and its rule that every offset is tried at most once. Its one-way phase 1, one-count phase-2 moves, constraint checks, blame order and mandatory confirmation cycle remain. The normative rule is [Phase 2](../spec/tuner.md#phase-2).

## Context

In the observed first round, journal sequences 37530–40877 covered 139 trials and 4.60 hours. Core 06 failed; cores 00, 08, 09, 11, 12 and 13 yielded and were permanently finished despite passing their own checks. Rollback A conflated returning to a previously passed profile with evidence that an innocent mover's offset was unusable. The owner decided on 2026-10-17 that a failed round must keep those movers as candidates ([#968](https://github.com/shgew/togi/issues/968)).

ADR 0054's simulator comparison selected rollback A over a variant that kept unloaded movers deepened. It did not compare yielding without permanent exclusion. Those historical measurements do not measure this rule.

## Decision

- Keep the conservative profile rollback: every deepened mover other than the blamed core yields to its round baseline, loaded or not. A yield answers no failure, creates no new failure point and queues no rerun. It no longer finishes its core.
- Keep every unblamed mover eligible under the ordinary solo-limit, failure-point and accumulated-combination checks. This includes movers whose checks passed, were partial or had not started. Eligibility is not evidence: an untested mover owes all its checks, and a tested mover owes whatever matching valid ledger evidence cannot answer.
- Preserve a same-offset retry's original alone workloads and evidence window. Extend the existing bounded per-core mark with its proposed offset and latest attempt round, retaining the original round and start sequence when that proposed offset is retried. A different proposed offset starts a new origin; a passed round removes the origin. Keeping the latest attempt separately lets the operator's phase plan distinguish a real new move after a yield from an unverified offset carried in place.
- Re-derive R7 checks from the new proposed profile. Their live-evidence window begins at the earliest deepened core's check origin. Reuse only matching trial classes whose profiles cover the new check profile, subject to failure invalidation and backend identity; carried evidence keeps its existing rules. A failed shared check is not accepted merely because its other loaded cores passed alone.
- Keep blame and backoff unchanged. The blamed core records a failure point and returns at least to its round baseline and one count shallower than that point. That forbids its blamed offset, not innocent movers' offsets. Reruns finish before another round begins. Known failures, ordinary backoffs, hunts and combination constraints retain their existing paths.
- Keep reset and resume deterministic. Marks are bounded by the core count and rebuilt by the pure journal fold, not a history query or a new event. A reset clears phase state and all check origins and returns to phase 1; the shared Ruleset 12 transition handles older sessions. This layer changes neither the ruleset nor the journal schema constant.

## Finite progress

A newly planned round cannot deepen into a recorded failure point or combination. If that round fails and blames a mover, its failure point forbids at least that proposed offset. The next round cannot recreate the failed proposed profile: the blamed move is constrained even though innocent movers may try their yielded offsets again. Several already queued failures are consumed individually; yields consume none of them.

Without reset, a core's failure point can only become shallower within the finite range [-50, 0]. A passing round advances at least one movable core toward its finite solo limit, or verifies an offset already carried in place. Ordinary failure handling repairs the profile and constrains failed offsets or combinations before rounds resume. A combination-blocked move is skipped, and it can become available when another member backs off; eligibility never bypasses the accumulated-profile check. Thus innocent retries do not create a rollback-only loop. Once there is neither an allowed move nor an unverified carried offset, the mandatory confirmation begins and rounds never reopen. Arbitrary inconclusive launches, external resets and hardware failures do not admit a wall-time guarantee.

The displayed round bound is conditional on no further failures, not a prediction of retry cost. Repeated failures can add rounds while adding constraints; passing attempts make finite forward progress.

## Alternatives

- **Keep rollback A's permanent finish:** rejected by the owner. Another core's failure is not evidence that the passing movers must lose their remaining margin forever.
- **Keep innocent offsets applied:** not selected. This decision changes eligibility, not the rollback profile; the next attempt runs with the blamed offset constrained and R7 checks derived again.
- **Keep only movers that completed their checks:** rejected. Check order would permanently exclude a later mover merely because an earlier one failed first. Untested movers remain candidates without being credited any pass.
- **Blindly rerun every own check or rotate its workloads after a yield:** rejected. It throws away uncontradicted work. The original bounded mark and the existing evidence ledger retain only valid credit without storing per-round history.
- **Accept a mover solely because its alone checks passed:** rejected. The round still needs its affected R7 checks at a covering profile and the full confirmation cycle.

## Evidence and checks

The target observation motivates the change; it does not establish a performance or hazard improvement. Deterministic regression cases cover untested movers retrying, passed alone and R7 checks retaining valid credit, repeated failed rounds exhausting blamed moves, combination blocking, a different offset selecting a new workload origin, reset clearing that origin, phase-plan move/carried classification and fresh-fold equality at every event prefix. These checks and the simulator/benchmark comparison must be run by integration; none was executed while implementing this layer.
