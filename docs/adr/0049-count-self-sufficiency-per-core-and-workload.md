# Count self-sufficiency per core and workload

Amends [ADR 0038](0038-self-sufficient-cores.md): its self-sufficiency ledger. [Issue #576](https://github.com/shgew/togi/issues/576) records the decision; [#506](https://github.com/shgew/togi/issues/506) reported the count as a defect and was closed as not planned.

## Context

ADR 0038 says the self-sufficiency ledger is kept per core, workload and intended duration and counts failures at equal-or-shallower offsets where the core was top or named. The tuner counts it differently, and the spec says so ([R7 self-sufficiency evidence](../spec/tuner.md#r7-self-sufficiency-evidence)): `R7Status` sums R7 passes per core and R7 workload across durations and loaded sets, counts only passes where the core was a top requester at an offset equal to or deeper than its current one, and never counts failures.

## Decision

Self-sufficiency is counted per `(core, R7 workload)`, across durations and loaded sets, and counts passes only. A core is self-sufficient for a workload once it counts one such pass. Failures do not enter the count: they invalidate evidence and move offsets through [R7 voltage-targeted backoff](../spec/tuner.md#r7-voltage-targeted-backoff), so counting them again would add nothing, and no decision reads the count. The spec owns the rule; this ADR only records that it, not ADR 0038's wording, is intended.

## Considered options

- **Per intended duration, with failures (ADR 0038's wording):** rejected. The count only reports breadth in `status` and the dashboard, a pass at any duration or loaded set where the core was top shows it can carry the rail, and failures never tolerate a trial whatever the count says.

## Consequences

`status` and the dashboard keep the spec's count. [#506](https://github.com/shgew/togi/issues/506) was closed as not planned because this count is intended. No code, ruleset or journal schema changes.
