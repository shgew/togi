# Later failures step a core back in pairs

Amends [ADR 0054](0054-two-phases-replace-the-deepening-loop.md)'s rule that after phase 2 concludes "failures only step cores back": they still only step back, now a core steps back on its second failure within a window. The rules are in [Later failures](../spec/tuner.md#later-failures) and [BIOS profile](../spec/tuner.md#bios-profile). [Issue #528](https://github.com/shgew/togi/issues/528) records the change and [#493](https://github.com/shgew/togi/issues/493) the decisions (Q22, Q30); it builds on [ADR 0054](0054-two-phases-replace-the-deepening-loop.md) and [ADR 0055](0055-rederive-r7-chains-after-a-backoff.md).

## Context

After phase 2 concludes, checking runs indefinitely and every failure backs a core off at once. Over a long run, single background failures wear the profile down, and nothing deepens it again. #493 settled two things in Q30: a failure in the profile shown for BIOS replaces it at once with the stepped-back profile, marked unconfirmed until a cycle passes; and once indefinite checking has passed cycles, a core steps back only after two failures within K cycles, with K set from measurements. [ADR 0004](0004-find-only.md) is unchanged: togi only reports a profile, and BIOS applies it.

## Decision

- **A gate.** The rule is active only when phase 2 has concluded and the current profile has passed a full cycle since it last changed: the latest passed full cycle ended after the conclusion and after the latest `profile.change`, and no decided move is waiting for its `profile.change`. Otherwise every failure follows the ordinary rules: the first cycle after the conclusion, and the time after any step-back until the stepped-back profile passes a full cycle. A reset closes the gate until the next conclusion.
- **Failures count per core.** A live failure observed after the gate opened counts against the cores the ordinary rule would step back for it, its strike set: its core when attributed; the core each voltage-targeted R7 decision would move (one per CCD); the loaded cores of an unattributed failure, every core for an idle one. Any load, class or regime pairs. Known-failure skips, carried failures, failures at CO 0, dead ends, hunt-phase outcomes and a located hunt's outcomes are never held.
- **Hold.** The first failure against a core is held: no offset, pass, failure point or rerun obligation changes. An attributed or R7 hold is a `tuner.decision` with phase `checking`, decision `backoff` and `from_offset == to_offset`; an unattributed one is a `hunt.skipped`. Reasons start `holds` and `held`. The fold tells a hold from an answer to a failure at a deeper offset ("already shallower") structurally: a hold answers a failure that applied the core's current offset, and a held `hunt.skipped` is one whose failing profile reaches no recorded constraint.
- **Pair.** A second failure against a core within K cycle starts of the first, while that core still sits at the offset the first found it at, steps it back by the ordinary rule. The oldest qualifying hold is used and each pairs once. Passed cycles between them do not reset a hold; moving the core for another reason or the window passing drops it. A pair applies even when the gate has closed in between. The decision cites the earlier failure last in `cause` and its reason names both failures and cycles; a paired hunt's `hunt.start` cites both and every decision or combination that ends it says `after failures #A and #B`.
- **A held failure is re-tested, not skipped.** A hold leaves the profile unchanged, so the failed class's passes since that failure are contradicted and its cycle requirement must pass again at the same profile. The pre-scheduling known-failure rule would otherwise skip that re-test and answer with an ordinary backoff that cites the held failure, so a held failure is excluded from it. The re-test is the fast path to the second failure, within the same cycle.
- **Holds do not count toward escalation.** Nothing was backed off ([ADR 0053](0053-escalation-only-located-hunts.md)). A pair counts as any backoff does.
- **Crashes are failures like any other** and are held and paired the same way.
- **The BIOS profile is derived and shows a held failure stepped back.** The last confirmed profile is the profile of the latest passed full cycle at or after phase 1's end. Each core shows the shallowest of its confirmed offset, its current offset and the targets of the valid holds naming it: deepened phase-2 moves stay hidden until confirmed, a step-back replaces the shown offset at the decision that records it, before its `profile.change`, and a held failure shows at once the offset the ordinary step-back would have moved its core to, which for R7 is the multi-count voltage-targeted target, marked unconfirmed. Reason: Q30's first sentence says the shown profile never carries an unresolved failure, and its second says only that the tuner itself does not step back on one failure. An unattributed hold (`hunt.skipped`) shows every core the skipped hunt would have searched, its candidates (the failure's nonzero members, computed by one helper that `huntStartNext` also uses), one count shallower; cores at 0 stay and the hunt stays skipped. Reason: hunting at once would step a core back after a single failure, breaking Q30's second sentence, and spend hunt crashes; the attributed case already shows a stepped-back profile the tuner is not testing, so this is consistent with it. The shown target stays while the hold is valid; a second failure within K pairs as above and the shown profile follows the actual offsets; moving the core for another reason drops the hold; when K cycle starts pass with no second failure the hold expires and the core shows its held offset again, which has passed those cycles, so it is confirmed again. The target is derived without a journal field: when the fold applies a hold it recomputes the ordinary decision from the state `Next` decided from, which only touches caches, and stores the target with the hold. `State.BIOSProfile` derives it from the journal; the operator display and `state.json` are [#529](https://github.com/shgew/togi/issues/529)'s.
- **K is 5.** See Measurement.
- **No new journal surface.** No event kind or field is added and `journal.Schema` stays 4 and `tuner.Ruleset` stays 10 until the ruleset bump of [#530](https://github.com/shgew/togi/issues/530). The state is a fold of existing events, so a resume equals a fresh fold. The journal `msg` of a hold reads like an already-shallower backoff.

## Considered Options

- **Immediate step-back after conclusion (the base):** rejected by #493 Q30, which decided the pairing.
- **Pair only failures of the same load or class:** not adopted. Q30 says a core steps back only after two failures, and instability spread across loads is still instability of that core.
- **Pair any two failures anywhere:** not adopted. It steps back more cores and wears the profile.
- **Count passed cycles or hours instead of cycle starts:** not adopted. A cycle start is one journal event, so the count is the same on a resume.
- **Hunt first, hold at the culprit's backoff:** not adopted. Every background idle failure would still run a hunt, and a fallback combination from one background failure is the wear Q30 forbids.
- **Keep the failed offset in the shown profile, marked unconfirmed (the former D1):** rejected. It shows a profile carrying an unresolved failure, which contradicts Q30's first sentence. The reason it was first taken, that a multi-count R7 target has no field, did not hold: the target is recomputed from the ordinary decision at the fold.
- **A new decision kind or field for holds:** rejected; `journal.Schema` stays 4.
- **A one-time milestone instead of a gate on the current profile:** not adopted. It would hold failures on a profile that has never passed a cycle.

## Consequences

- A real failure of a core waits for its second failure inside the window before the tuner steps it back. The shown profile does not wait: it carries the stepped-back offset, marked unconfirmed, while the hold is valid, and returns to the held offset when the hold expires.
- A held attributed failure still carries as a failure point into a later seeded session, because the carry rule reads the `failure` event, not the decision ([journal.md](../spec/journal.md#transitions)). Within the session a hold records no failure point, so a seeded session is more cautious than the session that held. A held unattributed failure carries as a failure fact like any other.
- One extra re-test of the failed class per hold, run in the same cycle.
- `--cycles 1` is unaffected: the gate needs a passed cycle after the confirmation, and `--cycles 1` stops at the next cycle start. `just bench` runs at `--cycles 1`.
- The state grows by one entry per hold, dropped after K cycle starts or when its core moves.

## Measurement

K is measured on multi-cycle simulated runs, because `just bench` stops after one clean cycle and the rule is inactive there. `tools/sim` was run directly, with no tracked tooling change, from a scratch build of each variant: the tree below #528 (the base, commit `b99b3c45`, the head of #489) and #528 on it with K = 1, 2, 3 and 5; K = 8 and an unbounded window ran as diagnostics. The numbers below were measured on that tree.

```sh
go build -trimpath -buildvcs=false -o sim ./tools/sim
sim --seed SEED [--machine tools/bench/MACHINE] [--replay-facts] --state-dir DIR --cycles 10 --max-boots 5000
```

`MACHINE` and `--replay-facts` are the scenario's entries in `tools/bench/suite.json`; SEED runs over each scenario's dev and holdout seeds (the dev seeds for `default`) for the eight scenarios the table and its closing paragraph name, which gives 144 sessions per variant: `default` 24 (dev only), `shared-voltage`, `target-shared-voltage`, `target-r7-vf-boost` and `target-r7-request-gap` 24 each (12 dev and 12 holdout), and `flat-hazard`, `late-onset` and `idle-limit` 8 each (4 and 4). The suite's `target`, `misleading-mce` and `target-nonmember-mce` scenarios did not run. Ten clean cycles is the confirmation plus nine cycles of indefinite checking, about 200 hours of simulated time. All 144 sessions of every variant stopped on the cycle count with no dead end. A throwaway program replayed each journal with the bench's scoring helpers; it is not committed. A variant's shown hazard is the time-weighted worst R7 hazard (the hazard of the worst loaded set, per hour) of the BIOS profile from the conclusion to the end, and wear is the sum of counts stepped back since the confirmed profile.

The rule for K was written before any run. It chose the smallest K in {1, 2, 3, 5} that met four criteria: every session concludes; the shown hazard's median and p90 on each shared-voltage scenario are not above the base's beyond a 95 % paired bootstrap (10 000 resamples, PCG(1,1), as the gate does; it fails only when the interval's lower bound is above 0) and the pooled final hazard stays within the Q24 bars (4.91 median, 10.90 p90); the median crashes after the conclusion are not above the base's by the same test; and in `late-onset` the latency to step back is at most K cycles with no miss.

Outcome:

- **Rule 1** holds for every variant.
- **Rule 2** fails for K = 1 on all three target scenarios, for K = 2 on `target-shared-voltage` and `target-r7-request-gap`, and for K = 3 on `target-shared-voltage` (lower bound +0.001); K = 5 passes all three. On `shared-voltage` every K has a lower bound of floating-point zero (5.6e-17), which this ADR reads as no excess.
- **Rule 3** holds for every K.
- **Q24 bars** hold for every variant: the pooled final worst R7 hazard is 0.96 / 1.54 per hour (median / p90) for the base and 1.24 to 1.52 / 1.96 to 2.49 for K = 1 to 5, against 4.91 / 10.90.
- **Rule 4** had no data: `late-onset` has no failure after the conclusion in any variant, so no latency or miss was measured.

K = 5 is the smallest K that passes. K = 8 and the unbounded window passed the same criteria as K = 5 and their shown-hazard medians match; other metrics differ slightly (on `target-shared-voltage` the final hazard is 1.21 for K = 5 and 1.31 for the larger windows, and crashes 36.0 and 37.5; on `target-r7-vf-boost` the shown-hazard p90 is 2.52 and 2.57). Ten cycles cannot separate K of 5 or more by the rule; K = 5 keeps the window bounded, as Q30 requires. K is not fitted to any one machine's history.

An earlier run of the same sweep, on a tree without #527's review changes to phase 2's failed rounds, found no K that passed rule 2: on `target-r7-request-gap` every K's lower bound stayed above zero (+0.02 at K = 5), because a hold by construction leaves a failing core at its offset until a second failure. The base is excluded by Q30, so K = 5 was chosen then as the K with the smallest excess shown hazard among those that passed the other rules (K = 1 failed rule 3 there). The rerun above, on the final tree, confirmed that choice under the rule.

**Shown hazard and the display fix.** The shown-hazard criterion and the table's shown hazards were scored under the earlier display, where a held failure kept its offset. Decisions and journals are unchanged by the display fix, so final hazard, wear, step-backs, holds, pairs and crashes stand. The new display is expected to lower the shown hazard while a hold is valid, an expectation that assumes hazard rises with depth, not a measured result; it was not rerun, and K was not re-chosen. A larger K is the conservative side: a wider window steps cores back more readily.

Median over seeds, with the shown hazard as median / p90 and the final hazard as median, in hazard per hour; crashes are those after the conclusion.

| Scenario | Variant | Shown hazard | Final hazard | Wear | Step-backs | Futile step-backs | Holds | Pairs | Crashes |
|---|---|---|---|---|---|---|---|---|---|
| `target-shared-voltage` | base | 1.83 / 2.60 | 0.95 | 45.5 | 41.0 | 25.0 | 0 | 0 | 38.0 |
| | K = 1 | 2.02 / 3.12 | 1.47 | 24.0 | 22.0 | 12.5 | 20.5 | 4.0 | 39.0 |
| | K = 2 | 2.01 / 2.96 | 1.41 | 30.0 | 26.0 | 14.5 | 17.0 | 6.5 | 38.0 |
| | K = 3 | 1.98 / 2.94 | 1.37 | 30.5 | 27.0 | 16.5 | 14.0 | 7.0 | 36.0 |
| | **K = 5** | 1.94 / 2.81 | 1.21 | 31.0 | 27.0 | 18.0 | 13.0 | 8.5 | 36.0 |
| | K = 8, unbounded | 1.94 / 2.81 | 1.31 | 31.0 | 28.0 | 18.0 | 12.0 | 8.5 | 37.5 |
| `target-r7-vf-boost` | base | 1.69 / 2.27 | 0.97 | 39.0 | 36.5 | 22.0 | 0 | 0 | 33.0 |
| | K = 1 | 1.91 / 2.62 | 1.54 | 18.5 | 17.5 | 10.5 | 20.5 | 4.0 | 35.0 |
| | K = 2 | 1.85 / 2.56 | 1.37 | 25.0 | 24.0 | 13.0 | 15.0 | 6.0 | 36.0 |
| | K = 3 | 1.82 / 2.56 | 1.32 | 25.5 | 25.5 | 13.5 | 14.0 | 6.5 | 37.0 |
| | **K = 5** | 1.81 / 2.52 | 1.22 | 27.5 | 27.5 | 14.0 | 12.0 | 8.0 | 37.0 |
| | K = 8, unbounded | 1.81 / 2.57 | 1.22 | 27.5 | 27.0 | 14.5 | 12.0 | 8.0 | 37.0 |
| `target-r7-request-gap` | base | 1.65 / 2.52 | 0.96 | 42.5 | 39.5 | 33.5 | 0 | 0 | 37.0 |
| | K = 1 | 2.08 / 2.86 | 1.54 | 22.5 | 20.5 | 16.0 | 22.5 | 5.0 | 38.5 |
| | K = 2 | 1.90 / 2.89 | 1.48 | 28.5 | 26.5 | 21.0 | 17.0 | 7.0 | 38.0 |
| | K = 3 | 1.84 / 2.87 | 1.52 | 29.5 | 28.5 | 23.0 | 13.5 | 8.0 | 37.0 |
| | **K = 5** | 1.84 / 2.87 | 1.46 | 31.0 | 29.0 | 24.0 | 13.5 | 9.0 | 37.0 |
| | K = 8, unbounded | 1.84 / 2.87 | 1.46 | 31.0 | 29.0 | 24.0 | 13.0 | 9.0 | 37.0 |
| `shared-voltage` | base | 0.35 / 0.51 | 0.10 | 1.0 | 1.0 | 0 | 0 | 0 | 1.0 |
| | K = 1 | 0.45 / 0.58 | 0.45 | 0 | 0 | 0 | 1.0 | 0 | 0.5 |
| | K = 2, 3, **5** | 0.45 / 0.58 | 0.45 | 0 | 0 | 0 | 1.0 | 0 | 1.0 |
| | K = 8, unbounded | 0.45 / 0.58 | 0.45 | 0.5 | 0.5 | 0 | 1.0 | 0 | 1.0 |
| `flat-hazard` | base | 0.81 / 0.81 | 0.81 | 125.5 | 125.0 | 125.0 | 0 | 0 | 156.0 |
| | K = 1 | 0.81 / 0.81 | 0.81 | 96.0 | 96.0 | 96.0 | 30.5 | 14.5 | 146.0 |
| | K = 2 | 0.81 / 0.81 | 0.81 | 96.5 | 96.0 | 96.0 | 22.0 | 15.5 | 144.0 |
| | K = 3, **5**, 8, unbounded | 0.81 / 0.81 | 0.81 | 95.5 | 95.0 | 95.0 | 23.0 | 17.5 | 150.0 |

A step-back is futile when it lowers neither the worst R7 hazard nor the highest hazard of any regime. On `flat-hazard` every step-back is futile by construction, so holds cut wear there by about a quarter. `default`, `idle-limit` and `late-onset` have no step-back after the conclusion in any variant. The gate of #570 was not scored.
