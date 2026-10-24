Status: superseded by [0080](0080-per-ccd-first.md); named-core voltage-target backoff and its count cap are removed.

# Cap named non-crash R7 backoffs

Amends [ADR 0038](0038-self-sufficient-cores.md)'s live named voltage-targeted backoff. [Issue #967](https://github.com/shgew/togi/issues/967) belongs to Ruleset 12 ([#684](https://github.com/shgew/togi/issues/684)). Decided by the owner on 2026-10-17; this is a layer of the shared ruleset change, not another ruleset or schema bump. [The tuner spec](../spec/tuner.md#r7-voltage-targeted-backoff) owns the rule.

## Context

Runs 4–5 of session 20261012T225227Z recorded named non-crash multi-core R7 failures whose passing-voltage targets surrendered much more depth than one rejected offset established:

| Failure sequence | Named core | Counts moved |
|---|---:|---:|
| 22810 | 00 | 18, −43 → −25 |
| 27225 | 11 | 11 |
| 27415 | 11 | 7 |
| 28347 | 12 | 13 |
| 29280 | 08 | 12 |

The targets were approximately 1.048 V, from passing loads where another core was the top requester. Such a pass establishes a usable rail voltage for that profile; it does not establish that the named failing core needs the whole gap. A failure point excludes the rejected offset and deeper values, so phase 2 cannot simply search back through it to undo an oversized backoff. These backend failures cost no reboot: trying a smaller move costs additional reruns, whereas surrendering depth can be permanent.

## Decision

Use a three-count maximum for the voltage-target estimate of a live multi-core R7 failure still charged directly to its named core, when its signal is `computation_error` or `unexpected_exit` and no crash was recorded while its trial was open. Keep the target-selection and evidence rules, and cite the target's passes even when the move stops short. Explain the uncapped estimate and the cap in the decision reason when the cap reduces it.

Three counts is a method constant chosen to make progress without accepting the full 7–18-count jumps in the motivating observations. It is not a calibrated stability boundary, an optimum established by benchmarking, or evidence that a three-count move will pass. The usual reruns answer whether it was enough.

The cap is applied before the existing failure-point constraint: the result must still be at least one count shallower than the shallowest recorded failure point and within [−50, 0]. Escaping a failure point can therefore move more than three counts. No passing target or no measured request still gives one count. Carried failures keep ADR 0064's no-target rule rather than taking three counts by default.

The cap follows effective attribution, not the mere presence of a core in the original failure. A named CO-0 non-top requester redirected to its CCD's request group takes that group's ordinary target and the full minimum required to step past a CO-0 top group. A named CO-0 top requester still requires the existing all-zero diagnostic before a dead end. Single-core R7, other regimes, unattributed charges and stall/MCE/crash signals do not qualify. A recovered trial is excluded even if recovery retains a computation-error or unexpected-exit signal from before its crash.

## Considered options

- **Keep the full named target:** rejected. The cited passes can belong to another top requester and the observed 7–18-count losses are not established as necessary.
- **Always move one count for these failures:** rejected. Three retains bounded progress toward useful passing evidence while avoiding the full jump. No experiment here establishes that one or three is optimal.
- **Cap every R7 backoff:** rejected. Unattributed failures use a different charge, crashes pay reboot cost, and a request-group stepdown may need more counts merely to exceed the old rail request.
- **Cap the final offset change even across a failure point:** rejected. That could leave a newly selected tuning profile at a recorded failure point, violating the existing constraint.
- **Infer non-crash solely from the failure signal:** rejected. Recovery can retain a backend signal after a crash; the fold already knows whether a crash interrupted the open trial.

## Consequences

The fold retains one crash fact with each pending failed R7 trial; it reads no historical journal and adds no event fields. Replay and resume reconstruct the same qualification and target cap. Smaller moves can add reruns and further failures before reaching a usable offset; those costs must be measured, not assumed away. The regression tests pin exact target-count boundaries, signal and crash qualification, failure-point precedence, zero routing, carried/no-target behavior and resume. This decision records no benchmark or target-machine validation result.
