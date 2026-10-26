Status: superseded by [0080](0080-per-ccd-first.md) and [0081](0081-margin-from-measured-slope.md); voltage-target backoff is removed, so its short-crash threshold is not a Ruleset 13 warm-up rule.

# Short crashes take no voltage target

Amends [ADR 0038](0038-self-sufficient-cores.md)'s voltage-targeted multi-core R7 backoff. [Issue #960](https://github.com/shgew/togi/issues/960) records the evidence and the owner's 2026-10-17 decision to include this rule in Ruleset 12. The [tuner spec](../spec/tuner.md#r7-voltage-targeted-backoff) owns the rule. The stack shares the ruleset change already made by [ADR 0064](0064-locate-when-passes-clear-the-loaded-cores.md); this layer changes neither compatibility constant.

## Context

The fold sees trial-end request summaries, not a passing trial's requests over a time window matched to an early crash. A passing target can therefore compare a full launch against only the beginning of a failed launch.

The recorded journal for session 20261012T225227Z, read without running a simulator, contains these core 03 request summaries for the CCD 0 `mprime AVX-512 36K-248K all-core` load:

| Trial | Core 03 offset | Last evidence | Request |
|---|---|---|---|
| 0784, 0790 | −14 | 32 s | 1.026438, 1.023650 V |
| 0808, 0836 | −14 | 48, 47 s | 1.036470, 1.036849 V |
| 0810 | −14 | 65 s | 1.038648 V |
| 0807, 0811 | −14 | 99, 117 s | 1.048487, 1.050138 V |
| 0837 | −10 | 15 s | 1.027666 V |
| 0843 | −4 | 218 s | 1.059776 V |

These are crashes. The same journal contains 42 passing 120-second launches of that load with core 03 at −14, ranging from 1.048095 to 1.051888 V. In particular, the 15-second crash at a shallower offset still has a lower request than those passes. The issue records the resulting six-count move from −10 to −4 at seq 21308.

[INFERENCE] Early mprime self-test work may account for the lower summaries. Neither that explanation nor an exact settling time has been established. Even the 65-second summary is still below the passing range.

## Decision

A live multi-core R7 crash with less than 60 seconds of recorded last evidence takes no passing voltage target. Use the ordinary one-count fallback, preserving the existing minimum needed to step down past a CO-0 top group, the escape past a shallower failure point, and the CO-0 clamp. Both named and unattributed crashes follow this rule; attribution, request-order fallbacks, escalation, located hunts and rerun obligations are otherwise unchanged.

A detected crash remains a crash when its end preserves a backend signal. Use the actual recorded end duration, never the intended trial-class duration. Zero timed evidence is below the floor. At exactly 60 seconds the ordinary voltage-target rule applies. Non-crash failures are outside this decision. Carried failures already take no passing target under ADR 0064.

Sixty seconds is a deliberately round heuristic between the observed 47–48-second early readings and the later 65/99/117-second readings. It blocks the clearly truncated sub-minute readings without asserting that every launch needs a full short-trial duration before its summary can be used. It does not correct the residual difference at 65 seconds and is not a fitted or universal warmup duration.

## Considered options

- **Matched elapsed windows:** rejected by the owner. Trial-end summaries cannot reconstruct them; storing and folding per-sample windows is outside this design.
- **Replace the crash reading with the newest passing measurement of its load:** rejected. It substitutes an earlier profile's estimate for the observed failing request and needs additional pass-only measurement state and backend/reset validity rules. One count already has a defined fallback meaning and needs no new cache.
- **Keep full voltage targets for every crash:** rejected. A lower truncated reading can turn an earlier crash into a larger irreversible backoff without evidence that more counts were needed.
- **Suppress every multi-count move:** rejected. CO-0 step-down must still put the selected core above the original rail; an existing shallower failure point must still be escaped. The floor suppresses only the passing-target contribution.
- **Use 120 seconds or fit a workload-specific floor:** not selected. The cited data motivates a sub-minute guard, not a universal full-trial requirement or a calibrated family of workload thresholds.

## Consequences

Only an optional crash-duration scalar is frozen with the existing failed-trial state. Replay rebuilds it from the trial end and preceding crash detection; no sample storage, journal field or history scan is added. A measured short-crash reason names its duration and the floor instead of citing a passing target it did not use. An offset-only response retains its existing fallback reason.

Repeated early crashes can require more launches and reboots before the profile settles. The implementation pull request reports the simulated cost and depth trade-off; it is not hardware validation. A counterfactual replay must retain the recorded attribution and request summaries to isolate the duration rule; fact-replay simulation does not preserve every attribution detail ([Simulating](../simulating.md#replaying-real-answers)).
