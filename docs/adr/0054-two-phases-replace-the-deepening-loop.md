# Two phases replace the deepening loop

Supersedes [ADR 0020](0020-hunt-and-refine.md)'s refinement loop toward the globally deepest profile, its halfway rounds and its stop rule (every core done, no further global improvement), and [ADR 0024](0024-schedule-from-uncontradicted-evidence.md)'s remaining credit for an uncontradicted earlier clean cycle toward `run --cycles`. The rules are in [Phase 1](../spec/tuner.md#phase-1) and [Phase 2](../spec/tuner.md#phase-2). [Issue #527](https://github.com/shgew/togi/issues/527) records the change and [#493](https://github.com/shgew/togi/issues/493) the decisions (Q17, Q21, Q22, Q29, Q31, Q33, Q34); it builds on [ADR 0053](0053-escalation-only-located-hunts.md).

## Context

Ruleset 10 overshoots, then deepens and backs off in a loop. On `target-shared-voltage` (24 seeds, [#493](https://github.com/shgew/togi/issues/493#issuecomment-6040930168)) it runs a median of 7 checking cycles, and its profile settles within one count of its final offsets only after 232 h. A passed cycle that ends with room reopens deepening toward the global optimum; each deeper profile must earn coverage again, and the next failure backs a core off again.

The owner does not want a loop that finds good-enough offsets, deepens, settles and deepens again (#493, Q17). The decision is two phases that end by themselves. The simulated experiment V6a-m1 on a throwaway research branch showed the shape: a one-way first phase with a one-count margin, then phase 2 at one count per round (median 46.1 h on `target-shared-voltage`, against 249.8 h). It also took a shortcut: when no core could move at the end of phase 1 it concluded at that cycle, without a confirmation cycle.

## Decision

- **Margin at search exit.** When a core's candidate solo limit passes its checks, the core enters checking one count shallower: `core.phase` from `search` records `offset` as the solo limit plus one, clamped to 0, and `pass` as the solo limit. Whether the core has room or is at its limit is evaluated at the margin offset. A solo limit of 0 has no margin and the core is never a phase-2 candidate. The margin is a method constant of one count ([#493](https://github.com/shgew/togi/issues/493), Q31 and Q1), not fitted to any machine. The tuner keeps each core's solo limit from that `core.phase` and a reset clears it.
- **Phase 1 is one-way.** Checking runs from the margin offsets until the first passed full cycle that ends after every core left search. Group failures only step cores back, by every existing backoff path, hunt and [ADR 0053](0053-escalation-only-located-hunts.md) escalation; phase 1 never deepens, and whether a core is at its limit no longer matters. That cycle's profile is the first confirmed profile.
- **Phase 2 is bounded.** A candidate is a core out of search with a recorded solo limit and an offset shallower than it. Each round moves every candidate one count toward its solo limit, in the session's CCD-alternating core order, accepting a move only if the accumulated profile reaches no failure point or combination (a recorded failure point or combination is never retried: invariant 4). A core whose next count is blocked is finished. Each round runs the existing deepening checks for the moved cores. Rounds repeat while any candidate can move; they number at most the largest candidate gap plus the number of failed rounds, because each failed round finishes at least one core. Q26's misblame filter does not apply: #493 Q33 supersedes it.
- **A failed round blames the deepened cores that were loaded.** The round ends. One core is blamed: the attributed failure's core if it was deepened; for a multi-core R7 failure, the named culprit if deepened, else the top request group among the deepened loaded cores, with ties broken by the less preferred core first, as R7 backoff does; otherwise the deepened core with the largest move. The other deepened loaded movers yield to their offsets from before the round; unloaded movers keep their moved offset and the next round re-checks them in place. The blamed core backs off to at least its pre-round offset and one count shallower than its new failure point. That backoff is the answer to the failure and consumes it.
- **Phase 2 always ends with a full confirmation cycle.** Once phase 1 has ended and no round is due, the next cycle is the confirmation cycle, also when no candidate could move at phase 1's end: phase 2 is then that cycle alone. A failure inside it whose failing profile has a loaded core deeper than its first-confirmed offset returns one such core per failure, top requester first, to its phase-1 offset with a failure point (#493, Q29); with no deepened core loaded, ordinary rules apply, so a phase-1 core steps back only when the failure recurs with every deepened loaded core reverted. If the confirmation ends without passing, the next cycle is still the confirmation; rounds never reopen.
- **The confirmation concludes phase 2.** Its first passed full end makes its profile the confirmed profile. Checking then continues indefinitely and never deepens again (#493, Q22): failures only step cores back.
- **Clean cycles count from the conclusion.** A clean cycle is a passed full cycle ending at or after phase 2's conclusion. The phase-1 cycle never counts, so `run --cycles 1` stops right after the confirmation and `--cycles N` counts further cycles of indefinite checking. Credit for an uncontradicted earlier cycle and the early end of a cycle a clean cycle already covers are removed.
- **A reset returns to phase 1.** `core.phase` to `search` clears the phase state, including that core's solo limit and the conclusion; the next phase-1 passed full cycle rebuilds it, and a new phase 2 and confirmation follow.
- **Phase-2 blames and confirmation reverts do not count toward escalation.** They are `deepening`-phase decisions that answer failures of deepened cores, not the unattributed voltage-targeted backoffs that [ADR 0053](0053-escalation-only-located-hunts.md)'s count of a load since its last pass is made of.
- **No new journal surface.** `has_room` and `at_limit` stay as descriptive core phases; no event kind or field is added. `journal.Schema` stays 4 and `tuner.Ruleset` stays 10 until the ruleset bump of [#530](https://github.com/shgew/togi/issues/530). `deepening.round.target` now holds the solo limits of the round's cores; the confirmation is an ordinary `checking.cycle` identified by order. The phase state is rebuilt from existing events, so a resume equals a fresh fold.

## Considered Options

- **No phase 2 (V3, V4):** rejected. It meets the time bar (43 h flat) but leaves the final profile's worst R7 hazard at 6.47 per hour against 4.85 for V6a-m1; the confirmation cycle of phase 2 lowers it ([#493](https://github.com/shgew/togi/issues/493), findings 3 and 5).
- **Halfway or bisection steps in phase 2:** rejected. A recorded failure point blocks every deeper offset, so gaps of three counts or more almost never occur and the step rule measured the same; one count per round keeps each offset tried once.
- **Q26's misblame filter:** rejected. It would let a core with one earlier group failure by another top requester retry that gap, which breaks invariant 4; Q33 limits phase 2 to taking back the margin and voltage-targeted backoffs.
- **Learned cycle weights (V5, Q34):** rejected. Search runs only R1 and R2 alone, so no R7 failure exists before the first cycle and the rule cut R7 to one occurrence in the cycle that sets the first result; the worst R7 hazard rose to 25.98 per hour. Every cycle runs the full configured cycle.
- **The early-conclusion shortcut:** rejected. V6a-m1 concluded at phase 1's passed cycle when nothing could move, which counted the phase-1 cycle as clean. Phase 2 always ends with its confirmation, so a run that stops after one clean cycle has always passed a cycle after the last deepening decision and every phase-1 core has been tested once more after the round decisions.
- **Rollback A, every deepened mover yields on a round failure (as V6a-m1 did):** not adopted. Its trade-off: it re-moves non-blamed co-movers to an offset they already tried, which breaks "each offset at most once".
- **Rollback B, only loaded movers yield:** adopted. It differs from A only in that unloaded movers keep their moved offset and the next round re-checks them in place. A loaded co-mover that yields still retries its offset in a later round, so B does not make each offset tried once either; the round bound above counts failed rounds for that reason.

## Consequences

- A run now ends phase 2 after at least one cycle that no deepening decision follows, and `run --cycles 1` costs that confirmation cycle after the first confirmed profile: the V6a-m1 median of 46.1 h on `target-shared-voltage` came with the early-conclusion shortcut, so a run that confirms takes longer.
- Phase 2 barely deepens: it takes back the margin and voltage-targeted backoffs of several counts. Most of its value is the confirmation cycle. A deeper offset that a recorded failure point forbids is never retried until a reset.
- A blamed core can end shallower than another core's co-moved offset would have allowed; a failure with no deepened core loaded still follows the ordinary backoff and hunt rules, so a phase-1 core can step back in phase 2.
- The ruleset change travels with the Ruleset 11 bump in [#530](https://github.com/shgew/togi/issues/530). Until then a development build that resumes a ruleset-10 journal folds its halfway rounds under the new rules; no compatibility code reads them.
- The operator surface (dashboard, `status`) still shows the old deepening and solo-limit display until its own change.

## Measurement

Simulated, `just bench --split all` (220 sessions), on the tree of this change over [ADR 0053](0053-escalation-only-located-hunts.md), no baseline scored. Every session of every scenario concluded. V6a-m1 is the experiment's figure on `target-shared-voltage`, which predates the current suite's overlays, so it is a reference, not a target.

| `target-shared-voltage`, 24 seeds | median h | p90 h | first passed cycle median h | median crashes | worst R7 hazard median / p90 per h |
|---|---|---|---|---|---|
| V6a-m1 | 46.1 | 61.7 | 43.4 | 193 | 4.85 / 7.57 |
| Rollback B (adopted) | 61.2 | 66.1 | 43.4 | 202.5 | 3.52 / 4.72 |
| Rollback A | 61.2 | 66.1 | 43.4 | 202.5 | 3.52 / 4.72 |

The median is 15.1 h above V6a-m1's and the p90 4.4 h above. The mandatory confirmation cycle follows the first result in every run, including the 9 of 24 where nothing could move at the end of phase 1 and phase 2 was that cycle alone; V6a-m1 concluded at the phase-1 cycle in that case. The first result matches the experiment's, the crash median is 9.5 higher and the hazards are lower. A run had a median of 1.5 rounds.

| Other scenarios, B | median h | p90 h | first passed cycle median h | median crashes | worst R7 hazard median / p90 per h |
|---|---|---|---|---|---|
| `target-r7-vf-boost` | 61.9 | 63.5 | 43.9 | 203 | 4.05 / 5.11 |
| `target-r7-request-gap` | 66.1 | 69.4 | 46.4 | 267 | 3.86 / 5.76 |
| `shared-voltage` | 55.7 | 57.1 | 28.7 | 24 | 0.46 / 1.38 |

Median time in hours, B against A: `default` 47.3 against 48.4, `target` 53.6 against 54.3, `flat-hazard` 102.0 against 107.4, `target-nonmember-mce` 49.8 against 50.8. The final profile's median depth is -525 on `late-onset` and -632 on `idle-limit`, with no difference between the variants there.

Rollback B was chosen over A because it is no worse on safety and faster: across the 96 sessions with a measured worst R7 hazard, the final profiles' hazards were identical; 33 of 220 final profiles differ; B finished 93 sessions sooner and A 7, by 0.7 h on average. B's blame and rollback decisions yield fewer cores (`flat-hazard`, mean per run: 12.6 yields against 40.0).
