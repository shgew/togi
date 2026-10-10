# Locate multi-core R7 failures only when backing off loaded cores stops helping

Supersedes [ADR 0040](0040-located-hunts.md) where it locates a multi-core R7 failure before charging the loaded cores; its all-zero rerun, located-hunt mechanics and Ruleset 10 gate record stay. [Issue #526](https://github.com/shgew/togi/issues/526) records the change and [#493](https://github.com/shgew/togi/issues/493) the decision (Q10).

## Context

ADR 0040 locates every live unattributed multi-core R7 failure whose failing profile has an unloaded core off CO 0 before it charges the loaded cores. On `target-shared-voltage` (24 seeds, [#493](https://github.com/shgew/togi/issues/493#issuecomment-6040930168)) located hunts cost ruleset 10 about 89 h of its 250 h median. Of a run's located hunts, 152.5 on average end `loaded`, after one crashing locate trial each: 84% of them end with ruleset 9's answer.

Experiment V1 in #493 removed located hunts outright. `target-shared-voltage` then took 57.4 h instead of 249.8 h, but `idle-limit`, whose cores fail while idle at shallower offsets than under load, ended at depth -371 instead of -662: its failures do come from unloaded cores, and without a locate the loaded cores took the blame down to CO 0.

The owner's decision (Q10): remove located hunts as the first answer, charge by ruleset 9's voltage-targeted backoff, and escalate to the idle cores only when stepping back busy cores stops helping.

## Decision

A live unattributed multi-core R7 failure is charged by [voltage-targeted backoff](../spec/tuner.md#r7-voltage-targeted-backoff), as in ruleset 9: against the loaded CCD's top group, or each CCD's top group for all-core loads, with the existing step-down past CO-0 groups. It is located ([Hunt](../spec/tuner.md#hunt)) only when stepping back busy cores stops helping:

- **After two backoffs.** Two voltage-targeted backoffs of the same load, the R7 workload on the same sorted loaded cores, have happened since that load's latest passing trial, whatever the trial's duration. The next unattributed failure of that load is located.
- **At once**, when every loaded core on the affected CCDs is at CO 0 while an unloaded core is not: no loaded core can step back, so a backoff has nothing to move.

The backoff count is derived from the journal: it is the `tuner.decision` backoffs that answered unattributed failures of the load, since the load's latest passing `trial.end` or `trial.carried`. Replay and resume rebuild it without a new journal field. An all-core failure that backs off one core on each CCD counts once.

The all-zero rerun before any `failure_at_zero` dead end is unchanged, and so is everything about a located hunt once it starts. A carried failure is charged as before. A failure attributed to the core it names is charged under the named-core rules and never counts toward escalation. A failure the existing CO-0 rules leave unattributed counts like any other unattributed failure of the load: one naming a CO-0 core on a CCD with no loaded core, or one whose all-zero rerun passed ([Escalation](../spec/tuner.md#escalation-to-a-located-hunt)).

Journal reasons let a reader follow the rule: each such backoff says how many backoffs of its load it is since the last pass and that a located hunt follows only after two; a located hunt names the backoffs that escalated it, or says that no loaded core can step back.

## Considered Options

- **Keep ADR 0040's locate-first rule:** rejected. About a third of the median time of the anchor goes to hunts whose answer is ruleset 9's, one crash each.
- **Remove located hunts outright (V1):** rejected. It reaches ruleset 9's time but loses the depth that idle-core failures need: `idle-limit` ends at -371 against -662.
- **Escalate after two backoffs, or at once when no loaded core can step back (V1e2):** adopted. [#493](https://github.com/shgew/togi/issues/493) measured depth -630 against -662 on `idle-limit` and -522 against -531 on `late-onset`.

## Consequences

- An unattributed multi-core R7 failure now costs a backoff and its rerun instead of a locate until its load has been backed off twice since its latest passing trial. A located hunt starts less often, so fewer locate trials are run, each of which usually ends in a crash ([ADR 0052](0052-crashes-are-a-cost.md) counts a crash as a cost).
- An unattributed failure caused by an unloaded core steps back the loaded cores' top group up to twice since the load's latest passing trial before it is located, and each pass restarts the count: fail, fail, pass, fail, fail, fail steps them back four times, so a loaded core can end shallower than its own limit; the located hunt finds the cause afterwards. The depth measured on `idle-limit` and `late-onset` bounds that loss.
- The ruleset bump that carries this change archives the session and seeds a new one ([ADR 0019](0019-a-ruleset-change-starts-a-seeded-session.md)). The Ruleset 10 gate stays as recorded in ADR 0040.
