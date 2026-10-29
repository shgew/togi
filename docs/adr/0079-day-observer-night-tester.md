# Days observe; nights test

Status: accepted for the Ruleset 13 stack (shipped in v0.12.0; its registered gate was not met). Its lifecycle is retained and current-policy ownership is consolidated in [0094](0094-two-ends-bounded-exploration.md); this record is historical.

The profile confirmed by the per-core policy froze during daily use, while its trials had little exact-profile exposure and omitted light work beside hot neighbours. [#1214](https://github.com/shgew/togi/issues/1214) therefore replaces phases, rounds, hunts, later-failure holds and reusable checking cycles with controlled night testing and a day observer whose profile advances only through the [tuner's pure day policy](../spec/tuner.md#day-steps). Daily use must not run a stress workload; the existing journal, machine seams, containment and SMU transaction remain the execution boundary.

Keeping the old policy as an alternative would retain competing definitions of evidence and promotion. A separate tuning project would duplicate the safety machinery. Both are rejected. [The scheduler](../spec/tuner.md#night-order-and-reserved-budgets) reserves baseline and re-qualification work so reproducing daily events cannot consume every night. The registered [day/night gate](../benchmarking.md#daynight-gate), not this decision record, owns evaluation thresholds.

This supersedes the per-core phase/checking policy of [0054](0054-two-phases-replace-the-deepening-loop.md) and [0056](0056-later-failures-step-back-in-pairs.md), including their later amendments. Historical timing results remain historical, not predictions for this schedule.
