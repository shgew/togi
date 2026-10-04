# Self-sufficient cores under shared voltage

Status: **Accepted**.

Supersedes [ADR 0023](0023-hunts-that-converge-on-shared-voltage.md) and the parts of [ADR 0020](0020-hunt-and-refine.md) about multi-core R7 hunts, joint/combination backoff and deferred noisy group testing. Their hunt mechanics remain available outside multi-core R7; R1–R6, R6 idle failures, single-core failures and their first-failure rules are unchanged. [Issue #105](https://github.com/shgew/togi/issues/105) records the approved decision; [#306](https://github.com/shgew/togi/issues/306) records the simulator prerequisites.

## Context

The two CCDs share VDDCR: the highest loaded-core voltage request sets the voltage. Offsets do not rank requests across cores: measured per-core bases spread about 47 mV on CCD0 and 28 mV on CCD1, equivalent to roughly 13 and 8 offset counts. Parking a core at zero can rescue the load and changes clocks, so a passing hunt group need not clear its members under the failing load. In ruleset 8, the CCD1 partial without core 10 failed four of four AVX-512 starts with computation errors on core 11 while its full part passed four of four. Of 62 ruleset-7 R7 failures, 43 crashed before 20 samples; the failing start often cannot measure its own request order.

ADR 0023 rejected “back off the member that sets the voltage, the shallowest one” as an unmeasured hardware explanation. We now choose the measured top requester, not the shallowest offset. Partial loads test the cores that its request otherwise protects.

## Decision

Ruleset 9 aims for every core to be **self-sufficient** under each R7 workload: passing as a **top requester**. Cores within 1 mV of the highest request tie. For each CCD, checking runs the full part, then a chain idling successive top groups while at least two cores remain loaded; all-core runs last. A one-CCD machine runs its full/all-core part once before its chain. Each part requires three short and one long passing starts and contributes ordinary evidence and full-lap coverage. Ruleset-8 record-only facts become ordinary evidence after the transition.

Requests come from the latest measurement of exactly the workload and loaded set, shifted to the current profile at 3.6 mV per count. An unmeasured partial uses the CCD's latest measured full part, restricted to its cores. Only absent telemetry permits offset order. Each derivation cites its source or explains that fallback. Derive a partial after its predecessor completes, freeze its set once started across resume, and re-derive unstarted parts after a profile change.

A named multi-core R7 failure counts against the named core. Otherwise count it against each affected CCD's top group; a persisted `stalled_core` restricts an unattributed all-core failure to that CCD without naming its culprit. There are no multi-core R7 hunts, parked groups, member probes or new combinations.

Tolerance is per core, workload and intended duration. Failures at equal-or-shallower offsets and top-requester passes at equal-or-deeper offsets, after reset, determine the one-sided binomial tail. Move only when that tail is strictly below `evidence.significance` (default 0.2) at `evidence.failure_rate` (default 0.05). A first-start failure moves, one in ten is tolerated, and two in ten move. A tolerated failure leaves passes and laps valid but does not fulfill its requirement. Same-BIOS carried facts count without aging and are evaluated before the first trial. Known R7 failures are not rerun as skips or counted twice.

Backoff targets measured passing voltage rather than an arbitrary stride. An unattributed top requester moves to the lowest higher top request where the same load passed n starts; a named core moves toward the lowest top request it passed under in that workload at an equal or higher CCD clock (no clock filter without failing clock telemetry). Counts round up at 3.6 mV per count, at least one; no qualifying pass means one count. Choose one lowest-ranked core per affected tied top group. Record its failure point and ordinary rerun obligations, citing the failure, request measurements and qualifying passes. Deepening rechecks R1/R2 alone, the core's top-requester partial for each R7 workload and the full CCD part when it is or becomes a top requester.

## Considered options

- **Offset-based m-th-deepest rule:** rejected because offsets misrank measured voltage requests across cores.
- **Keep hunts with idled cores:** rejected because changing the loaded set changes clocks and voltage; hunt passes do not answer the original condition.
- **One partial per CCD:** rejected because the next requester can protect further cores; the chain tests them in turn.
- **Per-class failure-rate bound:** rejected in favor of the core/workload/duration ledger, matching self-sufficiency and named failures across loaded sets.
- **Always back off one count:** rejected because measured passing voltage supplies a justified multi-count target; one count remains the fallback without a qualifying pass.
- **Tolerance as proof, requiring 16 clean starts per core:** rejected. Tolerance controls moves, not a stability certificate; conclusion remains one clean lap when requested.
- **Noisy group testing:** not pursued for multi-core R7 because the measured shared-voltage mechanism and partial chain replace those hunts rather than improve their group-test inference.

## Consequences

The ruleset bump archives the ruleset-8 session and seeds a new session from compatible carried facts; operators need not reset or transcribe them. Decisions expose request sources, tolerated failures, chain derivations and voltage targets. A clean lap is still breadth of tested workloads, not proof against rare failures.

Evaluation is gated, not assumed: on the 2026-10-04 R7 fold the fitted shared-voltage model must beat the constant on log loss and predict failures within a factor of two. On fitted development and holdout machines, worst final-profile hazard over R7 loads, partials included, must be lower than ruleset 8 with conclusion time at most twice as long. Crashes and depth are reported, not gated. A failed fit or bench gate stops for the owner's decision; the gate is not loosened.
