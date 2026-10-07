# Locate multi-core R7 failures before charging the loaded cores

Status: **Accepted**.

Amends [ADR 0038](0038-self-sufficient-cores.md): its multi-core R7 attribution, its `failure_at_zero` dead end "regardless of idle cores" and its bench gate. [Issue #415](https://github.com/shgew/togi/issues/415) records the decision and its design; [#416](https://github.com/shgew/togi/issues/416) the Ruleset 10 gate.

## Context

Ruleset 9 charges an unattributed multi-core R7 failure to the loaded CCD's top group and dead-ends once every loaded core of that CCD is at CO 0. ADR 0038's model has no failure source on unloaded cores. In the simulator, a core of the idle CCD that fails while idle, or a combination that fires whether or not its members are loaded, fails CCD0's whole-CCD load without a machine check naming a core. Ruleset 9 then backs CCD0 off one count per crash down to CO 0 and stops with "the instability is not caused by Curve Optimizer": `idle-limit` dead-ended in 8 of 8 runs of the committed ruleset-9 baseline, `late-onset` in 1 of 8, `target` in 9 of 36, all concluded by ruleset 8. In `idle-limit` dev seed 1 that is 300 consecutive R7 failures. The ruleset-9 gate reported those hand-set scenarios without gating them, so the regression merged.

ADR 0038 rejected multi-core R7 hunts because idling cores changes the loaded set, its clocks and the rail voltage, so a passing group need not clear its members under the failing load. Holding the loaded cores at their failing offsets and moving only the unloaded ones leaves all three unchanged.

## Decision

Ruleset 10 locates a live unattributed multi-core R7 failure whose failing profile has an unloaded core off CO 0 before charging the loaded cores. A **located hunt** runs the existing hunt machinery in the failed trial's own class. Its loaded cores are held at their failing offsets and are not candidates; its parked offsets are the failing profile with every unloaded core at 0; its candidates are the unloaded cores that were nonzero.

- Its first group, stage `locate`, reruns the failed load with every unloaded core at 0, at the failed trial's duration, and needs `n` passes like any group; a short pass does not clear a long failure.
- A failed locate ends the hunt `loaded`; when its failure names no core, the original failure gets ADR 0038's voltage-targeted backoff, or its zero-offset dead end. Only then does that dead end hold irrespective of idle cores.
- A passed locate is followed by unchanged delta debugging over the candidates: a singleton ends `culprit` and backs that core off, a combination gets member probes, a single candidate is the culprit at once. A group failure naming an unloaded core at a nonzero offset ends `direct`; one naming a loaded core, locate included, ends `loaded`.
- A `loaded` end after a group failure naming a loaded core charges that core as any named multi-core R7 failure would be: ADR 0038's backoff for the named core, with its failure point at its offset in the failed group trial. That one decision also answers the original failure, so no other loaded core moves for it. Charging only the original unattributed failure could move a different core of the CCD and leave the named core at the offset it failed at, to be reapplied.
- If narrowing finds no failing group, a `full` group then reruns the full failing profile at the failed trial's duration, since a short pass does not clear a long failure; an earlier full group at `short_trial_s` does not replace it. If it passes `n` trials, the hunt ends `loaded` too, charging the loaded cores as ruleset 9 does, rather than a `fallback` combination over cores that never failed. If it fails, it is an ordinary failed group, so an unattributed failure makes the candidates a combination with member probes rather than a `loaded` end.
- No loaded core moves while the hunt is open. Its end consumes the failure unless it is `loaded`; the commitment's ordinary rerun obligation reruns the failed load. A deepening round ends before the hunt starts.

Carried failures are still charged before the first trial, since a locate is a live trial. A failure that names a core, unless that core is at CO 0 on a CCD with no loaded core, a load whose unloaded cores are all at 0 already, and all-core loads keep ADR 0038's rules.

The Ruleset 10 gate keeps ADR 0038's gate on the anchored adversary scenarios and adds the hand-set scenarios `default`, `idle-limit` and `late-onset`: no run that ruleset 8 concluded may fail to conclude. The legacy `target` fits stay reported only; they fail the model check against the refreshed evidence.

## Considered options

- **Hunt the failure on the R6 idle path**, as a trial-less crash: it drops CCD0's load, which ADR 0038 rejects; its candidates include the loaded cores; and a failure that does not reproduce ends in a `fallback` combination.
- **Member probes over every unloaded core**: probes assume every member is needed, so an innocent core keeps failing down to 0, costing crashes and leaving a combination member at -1.
- **A standalone rerun trial**: it needs a new event, while a hunt group already resumes and infers from the ledger.
- **Changing only the dead-end branch**: it still spends about 300 crashes and all of CCD0's depth before reaching the cause.
- **An idle soak before R7 checking**: it spends time without a guarantee and misses combinations across CCDs.
- **Accepting the outcome and documenting it.**

## Consequences

The ruleset bump archives the ruleset-9 session and seeds a new session from compatible carried facts. Each unattributed multi-core R7 failure with unloaded cores off CO 0 costs a locate at the failed duration: up to `n` trials, ending at the first failure. A passed locate adds the narrowing's groups, each up to `n` trials and none for an inferred or skipped group, and, when none of them fails, up to `n` trials of the full failing profile at the failed duration. When the failure is rare and comes from the loaded cores, locate can pass by chance, and the narrowing that follows ends only once the full failing profile has passed `n` trials at the failed duration before the loaded cores are charged. The simulator keeps letting idle combination members crash, the conservative model, so a located hunt can find combinations of unloaded cores that the target machine may never show. #106 decides later whether those hunts move more than one count.
