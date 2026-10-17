Status: see [the index](README.md).

# Locate a multi-core R7 failure at once when passes clear its loaded cores

Extends [ADR 0053](0053-escalation-only-located-hunts.md) with a third ground for locating a multi-core R7 failure, and changes how a new session answers carried R7 failures ([ADR 0027](0027-carry-trial-facts.md)). [Issue #952](https://github.com/shgew/togi/issues/952) records the change; it is a sub-issue of Ruleset 12 ([#684](https://github.com/shgew/togi/issues/684)).

## Context

ADR 0053 charges an unattributed multi-core R7 failure to its load's top requesters and locates it on the unloaded cores only after two backoffs of the load since its latest pass. Its Consequences named the cost: when an unloaded core causes the failures and the load passes between them, every pass restarts the count and the loaded cores keep stepping back.

The fourth run of session 20261012T225227Z measured that cost on the target machine. Located hunt 14 passed its locate 5/5 (trials 0785–0789): the CCD 0 load `mprime AVX-512 36K-248K all-core` with its loaded cores at their failing offsets and every CCD 1 core at 0. The hunt ended with a CCD 1 combination. The same load then failed 6 of its next 20 trials with CCD 1 deep again, about one in six whatever core 03's offset, and each failure stepped back CCD 0's top requester, core 03, from −14 to −1 in five backoffs (seqs 21212–22350). The first two took their voltage target from the locate's own passes and moved 4 and 6 counts. Nothing in the ruleset carried forward what the locate had shown: that the loaded cores pass at those offsets when the unloaded cores are shallower.

A ruleset change starts a seeded session that evaluates every carried R7 failure before its first trial ([ADR 0019](0019-a-ruleset-change-starts-a-seeded-session.md)). It evaluates them after all carried facts have folded, so a carried failure's voltage target comes from passes recorded after it, at offsets later moves chose. Simulating that transition on a copy of the run's journal, one decision moved core 07 28 counts (−47 → −17) where the live session had moved it one, and search then ran 141 trials, 3.5 simulated hours, to walk the cores back to their failure points before checking began.

## Decision

- **Covered loaded cores escalate at once.** A live unattributed multi-core R7 failure whose failing profile has an unloaded core off CO 0 is located when `n` passes of its load, the R7 workload and the sorted loaded cores at any duration, hold every loaded core at least as deep as in the failing profile and some unloaded core shallower. The passes count under each class's active backend while valid under the evidence rules, and they are the ones valid when the failure folds, frozen like the backoff count: the count and the earliest witnesses up to the supported trial bound (1000). A pending failure rechecks the current `n` against that snapshot after a configuration change and cites exactly the earliest `n` frozen witnesses; no later pass, invalidation, reset or reload alters them. A pass at a profile the failing profile is within says nothing about the unloaded cores and never counts. When the backoff count also escalates the failure, `hunt.start` cites the backoffs; otherwise it cites the covering passes.
- **A covered carried failure needs no move.** A carried failure is not located on that ground. When its load's passes cover it the same way, it records no decision and stays pending: those passes show that its loaded cores need no move, and the load's live failures find the unloaded core.
- **A carried failure takes no voltage target.** It moves one count, or the step-down minimum its own trial's requests require when that is larger; its reason says so. The live session re-tests the moved core and answers its next failure with a live target.
- Both change decisions, so `togi_tuner::RULESET` goes from 1 to 2 and the active session moves to a seeded new one.

## Considered Options

- **Keep ADR 0053 as it is:** rejected. The run lost 13 counts on core 03 to failures the locate had already placed off the loaded cores.
- **Treat a covered live failure as needing no move, without locating it:** rejected. The failure still has a cause, and a locate finds it; when the locate fails, the hunt ends `loaded` and charges the loaded cores as before.
- **Count only passes at the failed trial's duration:** rejected. Escalation already pools a load's durations (ADR 0053), and the target run's covering passes were the locate's own, at the locate's duration.
- **Give a carried failure the voltage target of passes recorded before it:** rejected. The target index keeps per-profile counts and first witnesses, not every pass's sequence, and the live session re-derives targets anyway.
- **Change the rules without bumping the ruleset:** rejected. They change decisions, and a session's decisions follow one ruleset.

## Consequences

- `just bench --split dev` against `main` at `b156b1d4`, simulated, no gate registered. The generic verdict is REJECT (V2 4, V3 13, V4 1): scenarios whose failures come from unloaded cores gain, the R7 hazard scenarios lose a little.

  | Scenario | Seeds | Median hours | Mean crashes | Mean depth |
  |---|---|---|---|---|
  | `idle-limit` | 4 | 57.8 → 55.7 | 116.0 → 91.2 | −631.2 → −656.0 |
  | `late-onset` | 4 | 77.9 → 77.3 | 312.0 → 305.2 | −524.2 → −531.0 |
  | `misleading-mce` | 4 | 52.4 → 52.4 | 53.2 → 55.2 | −645.0 → −642.8 |
  | `target` | 18 | 51.2 → 51.2 | 84.2 → 84.8 | −660.9 → −660.3 |
  | `target-r7-request-gap` | 12 | 69.9 → 70.6 | 286.0 → 291.3 | −433.0 → −428.8 |
  | `target-r7-vf-boost` | 12 | 65.0 → 67.7 | 201.0 → 213.8 | −514.0 → −510.7 |
  | `target-shared-voltage` | 12 | 66.4 → 67.2 | 197.2 → 199.0 | −523.9 → −523.8 |

  `default`, `flat-hazard`, `shared-voltage` and `target-nonmember-mce` are unchanged. `idle-limit` regains most of the depth ADR 0053 gave up (−656.0 against ADR 0040's −662).
- Five passes are weak evidence against an intermittent failure: a load that fails one start in six passes five in a row 40% of the time. A covered failure whose cause is a loaded core then costs a locate, usually a crash, before the hunt ends `loaded` and charges the loaded cores as before. The converse, a failed locate repeating for every later failure of the load, stays with [#927](https://github.com/shgew/togi/issues/927).
- The ruleset bump archives the target machine's session and seeds a new one. Simulating that transition on the recorded journal, checking starts with core 03 at −12 instead of 0, and search before checking runs 77 trials (1.91 simulated hours) instead of 141 (3.5).
- A covered carried failure stays pending for good; only a reset of one of its loaded cores discards it.
