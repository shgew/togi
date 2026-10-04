# Tuner

Normative rules for how togi moves offsets. Terms are defined in `GLOSSARY.md`. Regimes, workloads and failure detection are in `workloads.md`; how decisions are recorded is in `journal.md`.

## Ruleset

The ruleset is the hardcoded strategy: search strides, offset range, phases, evidence, failure-point and combination rules, hunt and deepening, and checking coverage. Changing these bumps `tuner.Ruleset` (now 9) and archives an active older session into a seeded new session ([ADR 0019](../adr/0019-a-ruleset-change-starts-a-seeded-session.md), [ADR 0020](../adr/0020-hunt-and-refine.md), [ADR 0038](../adr/0038-self-sufficient-cores.md), [ADR 0024](../adr/0024-schedule-from-uncontradicted-evidence.md), [ADR 0027](../adr/0027-carry-trial-facts.md)). Changes to configurable defaults and fixes that record facts more accurately do not bump it.

The evidence epoch (`tuner.EvidenceEpoch`, now 1) separately versions compatibility of trial outcomes: workload content, backend binary or configuration, intended durations, and pass/failure detection (`workloads.md`). `session.start.evidence` records it. Without that field, a session with ruleset ≥ 6 has epoch 1; an older session has epoch 0. A transition drops passes from other epochs but keeps eligible failures. An epoch change is not a ruleset or journal-schema bump.

Transitions record eligible same-BIOS trial and idle-failure facts before `session.carried` (`journal.md`, Transitions). Ruleset 9 treats carried ruleset-8 `record_only` R7 outcomes as ordinary evidence, with no aging; it does not carry profile offsets or clean cycles. Carried multi-core R7 failures are attributed and evaluated before the first trial.

Only multi-core R7 changes in ruleset 9: whole-CCD, partial and all-core loads. R1–R6 evidence and search, R6 idle failures, single-core failures and their hunts, failure points and combinations from those paths, and their first-failure rules remain unchanged.

## Invariants

1. Every offset stays within [-50, 0]. togi never writes a positive offset.
2. During a trial run alone only the target carries a nonzero offset. Every other core is written to 0 first, including at the start of each boot, when firmware has restored the BIOS values.
3. Every SMU write is preceded by a durable intent event and followed by a readback. A readback that differs from the written value is a dead end.
4. No applied or restored profile reaches a recorded failure point or combination, except after reset.
5. Offsets get deeper only in search and in deepening rounds.
6. Every decision is an event that names its cause (`journal.md`).

## Session start

1. Preflight (`runtime.md`) passes, or togi stops at a dead end.
2. The first `run` of a session reads every core's offset from the SMU as the baseline and records the BIOS context.
3. Each core's start offset is the configured override if one exists, else its baseline, clamped to [-50, 0]. A configured candidate solo limit starts the core in search at that offset with `check_solo_limit` and frozen R1/R2 workloads; it still needs the full evidence rule.

   A session started by a transition (`journal.md`, Transitions) seeds cores from `session.carried`, citing that event after the baseline. Precedence: configured candidate solo limit, configured start offset, carried candidate solo limit, baseline. A carried candidate solo limit starts search with `check_solo_limit` and reason `candidate solo limit <candidate_solo_limit> carried from session <id>`. A carried failure point is recorded in the first `core.phase`; a start at or deeper than it is clamped one count shallower, regardless of its source. A failure point at 0 remains a dead end until reset.
4. A nonzero baseline produces a notice recommending BIOS CO 0 for tuning. togi still starts from it.
5. A later `run` whose BIOS context differs archives the old session and starts a new seeded session carrying candidate solo limits but not failure points and combinations (`journal.md`, Transitions).

## Scheduling

Cores are visited in CCD-alternating order: 0, 8, 1, 9, ... 7, 15. Each turn goes to the next core still in search. Its R2 trial follows a passing R1 in the same turn; an inconclusive trial retries the same class unless a pending reset supersedes it. Interleaving cores lets each cool between its own trials.

## Search

A search step runs one R1 trial alone then one R2 trial alone at the same offset, 90 s each by default. Its first failure rejects it. The eventual candidate solo limit additionally needs `n` passes in each of its frozen R1 and R2 trial classes, using `durations.search_trial_s`.

Carried passes can satisfy `check_solo_limit` in its frozen R1/R2 classes at `durations.search_trial_s`, even though they precede the solo limit-check phase boundary. Ordinary search steps still require live trials.

Each core tracks its current offset `o`, `pass` (the deepest offset with a passed step, or none) and `fail` (its failure point, or none).

After a passed step at `o`:
- `pass = o`.
- `o == -50`: check this candidate solo limit.
- `fail` is none: next offset is `max(o - 5, -50)`.
- Otherwise the next offset is `o - 1`. If that equals `fail`, check `o` as the candidate solo limit.

After an attributed failure at `o`:
- `fail = max(fail, o)`, keeping the shallowest failure.
- If `pass` is at or deeper than `fail`, it is discarded, because the new failure contradicts it.
- `o == 0`: dead end.
- `pass` is none: next offset is `min(o + 5, 0)`.
- Otherwise the next offset is `pass - 1`, or check `pass` as candidate solo limit if `pass - 1 == fail`.

Any failure during a trial run alone is attributed to the target, crashes included: no other core carries an offset that could explain it.

## Evidence

All conclusive trials and facts enter the evidence rules, including former `record_only` R7 outcomes. Every multi-core R7 failure requires a voltage-targeted backoff; other trials retain the first-failure rules.

One trial is one workload launch, with no internal relaunch. The pass rule is `n = ceil(ln(evidence.miss) / log1p(-evidence.rate))` passing trials, five at the defaults (0.05, 0.5). Outside multi-core R7 the first failure rejects a step and restarts the consecutive pass count. The full count applies to a candidate solo limit's R1 and R2 classes, every hunt group and every deepening check.

A trial class is `(regime, workload, sorted loaded cores, duration_s)`, using intended duration, not the elapsed time before a crash. The ledger records each conclusive trial's class, sequence, applied `trial.intent.profile` and outcome. A pass at profile Q counts toward P only when Q is at least as deep as P and follows the latest failure of that class at a profile at least as shallow as P; a failure at Q rules out P when Q is at least as shallow as P. Requirements on the same class within a step add, so a trial counts once. An idle crash has wildcard R6 class with all cores loaded and invalidates all such R6 classes. Passes at deeper profiles can survive backoff; deeper moves need new evidence.

The ledger folds `trial.carried` and `failure.carried` before live evidence, ordered by their sequence in the new journal and marked as carried. Carried passes count across the local sequence boundaries for candidate-solo-limit checks, hunt planning and outcomes (including projected pass counts), rerun obligations and deepening checks. Full-cycle coverage uses only live passes since its cycle start. Carried failures count everywhere failures count, regardless of those local boundaries; multi-core R7 failures are processed once for voltage-targeted backoff. Failures invalidate earlier covered passes and keep a class failing until `n` newer covering passes; outside multi-core R7 they also participate in monotonicity warnings. A core reset discards carried evidence under the same loaded-core rules as live evidence.

An evidence-backed decision cites the accepted carried events' sequences in the new journal, not their old sequences, and its reason names the original source sessions. Copying a fact does not create another trial or live exposure.

Before scheduling any trial outside multi-core R7, the tuner checks whether its class has a valid failure at a componentwise equal-or-shallower profile. A failure is valid until `n` newer passes in that class cover the scheduled profile. Both live and carried failures qualify; deeper, incomparable and different-class failures do not. The trial is not run: a `failure` decision records that it was skipped and why, cites the known `trial.end`, `trial.carried` or idle-failure sequence, and activates the same attribution, backoff or hunt path as a live failure. It adds no trial, exposure or new failure fact. Ordinary search still needs live passes, but its known failures need not be re-observed. A multi-core R7 trial is never skipped for a known failure: that observation has already been processed once for backoff.

A failure outside multi-core R7 contradicting `n` valid passes on a profile at least as deep in the same class records `tuner.warning` `monotonicity`, without changing the failure decision. An idle failure checks all-core R6 classes individually before its wildcard invalidation; if several qualify, the warning cites exactly `n` passes from the class with the earliest valid pass.

## R7 request order and attribution

The target is self-sufficiency: each core passes as top requester for each R7 workload. Loaded cores within 1 mV of the highest request are tied top requesters; an all-core load has a top group on each CCD. Request groups are ordered highest first, with ties within 1 mV of each group's highest request, not transitive adjacent differences.

For load `(workload, sorted loaded cores)` at profile P, obtain requests in this order:

1. The newest `trial.end` or `trial.carried` with `voltage_requests_v` for exactly that workload and loaded set, regardless of duration or outcome. Shift each core's request from the measured applied profile Q by `0.0036 * (P[c] - Q[c])` volts.
2. For a load never measured, the newest measured full part of its CCD for that workload, shifted to P and restricted to the loaded cores.
3. With no request telemetry for that CCD and workload, use `0.0036 * P[c]` volts as an offset proxy: shallowest first, equal offsets tied.

Every ordered decision cites its measurement sequences in the current journal, or explicitly says it fell back to offsets. The failing or passing trial's own `top_requesters` takes precedence for attribution and self-sufficiency evidence; otherwise derive its top groups using the order above on its applied profile with only measurements available as of that trial, never later measurements.

A multi-core R7 failure naming a core through a backend signal or exactly one core-local MCE counts against that core. An unattributed failure counts against the loaded CCD's top group, or each CCD's top group for all-core loads. When `stalled_core` identifies one CCD, only that CCD's top group is affected; it does not name the failing core. Multi-core R7 never starts a hunt, parked group, member probe or combination. These rules apply to live and carried failures and to any already-recorded known-failure skip, without duplicating an observation.

An unattributed failure whose affected top group is entirely at CO 0 steps down the same CCD's request order to the next group with a movable loaded core. It dead-ends with `failure_at_zero` only when every loaded core of that affected CCD is at 0, irrespective of offsets on another CCD or idle cores. A named core at 0 dead-ends only if it was in its own CCD's top group in that trial; otherwise route the failure to that CCD's top group, including the same stepdown rule.

## R7 self-sufficiency evidence

The ledger unit is `(core, R7 workload, intended duration_s)`, after the core's latest reset. It counts passes where the core was a top requester at equal-or-deeper offsets and failures at equal-or-shallower offsets where it was a top requester or the failure named it. Same-BIOS carried facts count with no aging, subject to reset exclusions, and carried failures are evaluated before the first trial. This ledger describes self-sufficiency and supports voltage targets; it never permits a failed trial without a move. One clean cycle still concludes a requested one-cycle run, without a bound on future stability.

## R7 voltage-targeted backoff

Every multi-core R7 failure requires a move unless the zero-offset rules above establish a dead end. Choose a target V* from profiles with `n` passing trials, where n is the Evidence pass count:

- For an unattributed failure, select the first request group on each affected CCD containing a movable core. Move one movable core in that group, the lowest-ranked by preferred-core ranking, then core-id order. Use the lowest top request above the failing top request among passing profiles of the same workload and loaded set. In a whole-CCD or partial load, use that CCD's numbers; in all-core loads, choose independently for each affected CCD. A named core at 0 that was not a top requester uses this path on its own CCD.
- For a named core X, use the lowest top request X has passed under in any load of that workload containing X, with `n` passes on that profile. Its CCD's passing median clock must be at least the failing trial's `ccd_mhz`; omit the clock filter when the failing trial has no clock telemetry.

The moved core's own failing request comes from its trial's `voltage_requests_v`, or the request-order fallback on its applied profile. Raise it by `max(1, ceil((V* - r) / 0.0036))` counts; without a qualifying passing target, use one count. When stepping down past a CO-0 top group with telemetry, also require at least `floor((r_top - r) / 0.0036) + 1` counts to put its estimated request above the failing top request. Take the larger count, then clamp the offset to [-50, 0]; the clamp can prevent reaching that voltage. Without telemetry, step down using offset order and apply the ordinary one-count fallback, without claiming that its physical request exceeds the top. The decision cites the failure, request measurements and passes defining V*, and explains the voltages, count and any offset-order fallback without printing invented voltages.

Record the moved core's failure point at its applied offset in the failed trial, keeping the shallowest point. Deepening cannot return there; ordinary backoff rerun obligations follow.

## Failure points and combinations

A failure point is reached when `P[c] <= fail[c]`; a combination is reached when every member `m` has `P[m] <= C[m]`. A core is at its limit at -50 or if one count deeper would reach a failure point or combination. Re-evaluate whether each core is at its limit after every failure point or combination or offset change. Failure points and combinations accumulate until reset.

Among safe profiles, choose the greatest total depth (most negative sum of counts); preferred-core ranking breaks ties, then core-id order. Outside multi-core R7, a combination backoff first looks for a tested move: a passing member probe of the hunt that moved one member with every other member at its failing offset. Of those whose move leaves the profile reaching no failure point or combination, it takes the one moving its member the fewest counts, breaking ties toward the lowest-ranked member, and moves that member to the probe offset, citing the combination and the probe. Without one, it chooses the member leaving the most depth reachable, breaking ties toward the lowest-ranked member, and moves it one count past the combination. Eligible individual failure points carry to a new same-BIOS session, not combinations; multi-core R7 records a new point for the core each voltage-targeted backoff moves.

## Trial alone sequence

Between trials every core is at 0. Before its first trial alone, a `run` writes every core to 0 with one set-all command and reads each back, then records `profile.applied` with condition `alone`. Pending decisions, a failure at 0 among them, are made before that write. One trial alone is then:

1. `trial.intent`;
2. SMU set target to its offset and read it back (including at 0);
3. `trial.start`, plus `trial.signal` with the load-step schedule for R3 and R4;
4. the trial, then `trial.signal` with the SIGSTOP and SIGCONT counts for R3 and R4;
5. SMU set target to 0 and read it back (including when the target was 0);
6. `mce` events for the trial window;
7. `trial.end`;
8. the tuner's `failure` when the trial failed.

## Together trial sequence

Together trials run on the profile of the last `profile.change`. Before the first together trial of a `run`, and after every profile change, each core is set to its profile offset in core-id order, each write read back, then `profile.applied` is recorded with condition `together`. Parked hunts apply their recorded group profile instead. One together trial is then:

1. `trial.intent`, naming its loaded core for R1 to R5, or the loaded cores in `cores` for R6 and R7;
2. `trial.start`, plus `trial.signal` for R3 and R4;
3. the trial, then `trial.signal` counts for R3 and R4;
4. `mce` events for the trial window;
5. `trial.end`, naming the backend instance's core when a backend signal ended it;
6. the tuner's `failure` when the trial failed.

No SMU write happens between together trials: the profile stays applied.

R6 loads every core. Each R7 checking step runs each CCD's full part first, then its partial chain, in CCD order; the all-core part runs last. On one CCD the full part is the all-core part and runs only once, before its chain. Each partial idles the predecessor's top requesters, including ties within 1 mV, leaving at least two loaded cores. Every full, partial and all-core part has three short trials at `short_trial_s` and one long trial at its CCD's full-part duration (counts add when durations coincide). Each trial has its own intent, instance set and teardown. Only loaded cores run backend instances; the profile offsets on all cores stay applied.

`checking.step` records the R7 occurrence's starting profile. Once the predecessor has fulfilled its passing-trial requirements, `checking.chain` derives the next partial from that load's current request order, citing the measurement sequences or explaining the offset fallback. It records the tie groups, next loaded set and derivation profile; an empty set records that removing the top group would leave fewer than two cores. A derived part's set is frozen across resume once any trial has been issued, including an inconclusive trial. After a profile change, parts not yet started re-derive before running. Partial outcomes are ordinary evidence for cycle requirements, full-cycle coverage, reruns, deepening checks and the R7 self-sufficiency ledger. Historical ruleset-8 `record_only` outcomes, live or carried, are ordinary evidence too; ruleset 9 emits no such marker.

## Crashes

A crash is classified by what its boot recorded. A together application counts as applied from its first nonzero `smu.intent`, before `profile.applied`. A backend signal recorded in `trial.progress`, a trial-window MCE selected by boot-local monotonic time or an uncorrected MCE in the next boot takes precedence over reset reason. Otherwise a thermal trip is a dead end; a power-button reset during a trial is its failure, but without an open trial is inconclusive. When reason reporting is supported and confirmed, no reason line means power loss and is inconclusive. An unknown or unsupported reset reason retains the usual classification:
- A trial in flight: a failure of that trial. Alone failures belong to the target; together and parked attribution follows Checking.
- An applied profile with no trial in flight: an idle crash, treated as an unattributed R6 failure with all cores loaded; a trial alone all-zero profile changes nothing.
- Nothing applied: a stray crash. Reaching `dead_ends.stray_crashes_in_a_row` is the boot-loop dead end.

Restoring offsets before `shutdown` (`runtime.md`) is not an application: its `smu.intent` events cite `session.baseline` and leave the boot's last application as it was, so a crash part-way through is classified by what was applied before. After `profile.restored` the cores are back at togi-independent values, and a crash counts as if nothing was applied.

`crash.detected` carries the condition of the boot's last application.


A crash with a trial in flight and an idle crash are failures like any other: no rule counts them against search or checking, caps them, or pauses after a run of them ([ADR 0018](../adr/0018-crashes-are-not-a-cost.md)). Only stray crashes are counted, for the boot-loop dead end.

## Decision events

Moves are `tuner.decision`: `step_deeper`, `check_solo_limit`, `deepen`, `yield` and `backoff`, with phases `search`, `checking`, `hunt` or `deepening`. `check_solo_limit` freezes the R1/R2 workloads; `core.phase` to `has_room` or `at_limit` records the checked solo limit, pass and failure point. Each decision cites its cause and reason. A search backoff says `failed`; other backoffs say `backed off`. A phase change after an offset or failure point or combination update re-evaluates whether the core is at its limit.

## Checking

Checking begins when no core remains in search. Its first `profile.change` has `from: null`. A profile change does not end an open cycle; passing steps on a deeper profile remain evidence after a backoff. Only `reset --core` and a covered cycle ending for deepening (Deepening) end a cycle without passing.

A cycle captures the configured schedule in its start event. R1 and R2 occurrences select successive catalog workloads (three occurrences cover each catalog); R3, R4 and R5 run on each core; R6 runs all cores. Each R7 occurrence selects its next R2 workload and requires every full, derived partial and all-core part's three short and one long passing trials. Requirements sharing a trial class add, including repeated partial classes, and trials passed on sufficiently deep profiles since the cycle start can fulfill them. The first unmet requirement of the first unmet step runs next; a partial is not derived until its predecessor passes. The cycle ends passed only after every chain is complete and every requirement passes, and is full when it also has at least three R1, R2 and R7 steps and one each of R3, R4, R5 and R6. A non-full end records the missing coverage. The newest passed full-cycle profile, or an older eligible one when it covers the failing profile and is nowhere shallower, supplies a hunt's parked offsets.

Only live passes since the cycle start fulfill cycle requirements; carried passes never fulfill them. Failure invalidation is global, including carried failures before that start, so the live-pass boundary does not restore contradicted evidence.

Outside multi-core R7, together and parked attribution uses the backend instance's reported core first, then exactly one core named by core-local MCEs, then the single nonzero core in the applied profile. An attributed failure records the failure point at the applied offset and backs off that core to at least one count shallower, including when it was held at a parked offset; failure at 0 is a dead end. If the core was already shallower, its offset need not move. An unattributed together or idle failure queues a hunt of the failing profile and trial class instead of backing off the loaded set. An unattributed parked failure is the group outcome. Multi-core R7 instead follows R7 request order, attribution and voltage-targeted backoff above. Inconclusive trials retry the same class.

A together checking requirement outside multi-core R7 with a valid known failure goes directly to that failure's attribution or hunt rather than starting the workload. Attributed skips retain the known failing offset for the failure point and back off beyond it; unattributed skips hunt the known full failing profile and class. The decision changes pending state exactly as a live failure would, so a skipped requirement cannot repeatedly skip without making progress. The same pre-scheduling rule applies to reruns and deepening checks outside multi-core R7; a failed deepening check closes its round before the backoff or hunt. Multi-core R7 never skips a trial for a known failure.

When a backoff or hunt commitment changes an offset, checking first reruns the failed class: `n` trials at `short_trial_s`, then one at its original duration if different. These obligations are FIFO. A rerun cites the newest queued failure of its class; this does not change the obligations or their evidence windows. `run --cycles N` stops only after N clean cycle ends valid for the current profile, with every core at its limit and no deepening able to improve total depth; without the flag checking continues indefinitely.

Carried passes can satisfy a rerun's class requirements despite preceding its obligation boundary; failures, including carried ones, retain their normal invalidation effect. When carried passes answer the rerun, the following checking-cycle or deepening-round decision cites those facts and names their source sessions. A rerun does not supply carried passes to full-cycle coverage.

A cycle counts only if every core was at its limit when it ended. Ends after the last deepening count as before. An earlier end can also count if it followed the latest `command.reset`, its ending profile was at least as deep as the current profile on every core, and no failure of any trial class since that reset occurred at a profile equal to or shallower than its ending profile on every core. An incomparable failure does not contradict it; a failure with an incomplete profile conservatively prevents this credit. This credit does not change the cycle-start evidence window.

`status` reports clean cycles since the last deepening: the count valid for the current profile, including eligible earlier credit, and the latest clean cycle's number. `run --cycles N` checks its stop rule before starting the next cycle. A clean cycle establishes workload breadth, not a guarantee against rare failures or untested real use.

## Hunt

Hunts are limited to failures outside multi-core R7, including R6 idle and single-core failures. The rules below retain their first-failure interpretation; R7's measured request attribution and voltage-targeted backoff do not apply to them.

Every unattributed together failure outside multi-core R7 is hunted unless its failing profile already reaches a recorded failure point or combination; then `hunt.skipped` explains why. An unattributed failure with every core at CO 0 has no candidate to hunt: it is the `failure_at_zero` dead end, naming no core. The hunt parks other cores at the newest passed full-cycle profile raised to the failing profile: each core takes the shallower of its passed full-cycle and failing offsets, so passes on the passed full-cycle profile cover parked offsets. A passed full-cycle profile that is nowhere shallower than the failing profile cannot supply parked offsets; the hunt falls back to older passed full-cycle profiles and finally all-zero. Candidates are precisely cores deeper at the failure than at their parked offsets.

For each group, candidates in the selected subset take their failing offsets and every other core takes its parked offset. Delta debugging tests parts, then complements as granularity increases, retaining a failing subset. By default it starts at `short_trial_s`; if all initial parts and complements pass, it tests the full failing profile, and if that too passes `n` trials, repeats at the failed trial's duration. At most one duration escalation occurs.

A hunt of a failed trial starts directly at that trial's longer duration when `n` valid `short_trial_s` passes of the same regime, workload and loaded cores cover the failing profile, recorded after the latest reset and before the failure, and no earlier failure in that regime at `short_trial_s` or less has occurred since reset. The short-failure veto includes other workloads and profiles. Idle failures without a trial do not use this prior. The first `hunt.group` explains the duration choice and cites the supporting pass sequences; the short passes do not establish an outcome at the longer duration.

With more than two candidates, a hunt can probe the core with the most recent failure point alone before its normal binary parts. The two newest eligible attributed parked failures of the chosen trial class before the hunt's source failure must both name that core, occur after the latest reset, and have adjacent offsets: the newer failure is one count shallower than the older, and the current failing offset is one count shallower again. The core must be a candidate. The first group cites both failures and explains the ordering. A failure is handled normally; a pass returns to binary parts at index zero and never establishes an untested group.

A group passes after `n` trials; its first failure rejects it. Ledger evidence can infer an outcome: a part or complement counts the session's evidence of its class recorded after the newest `command.reset` of any core, including earlier hunts'. The same boundary applies while the group runs and to its projected pass count, so earlier valid passes combine with new trials. Passes must be at an equal or deeper profile, and failures at an equal or shallower one, under the ledger's usual rules. Full groups and member probes infer only from evidence recorded since their hunt started and count new trials since their group started. A group reaching a new failure point or combination is skipped. `hunt.group` records the partition stage, subset, group profile, duration and outcome so interrupted hunts resume without duplicating commitments.

Carried passes and failures are admitted across these reset, hunt-start and group-start inference boundaries for every group stage, subject to reset invalidation and the usual class and profile rules. This applies both when planning an inferred group and when evaluating or projecting its running outcome.

The pre-scheduling known-failure rule also prevents a parked trial from re-running a valid failure outside its local inference window: an inferred-failure `hunt.group` cites the known failure and advances the existing group plan. This does not widen pass inference windows. A hunt entered from a skipped together trial records the known failure's sequence, original trial identity, full failing profile and class in `hunt.start`, including when the source is `trial.carried`; its message explains the skipped trial and source session.

A singleton ends `culprit` with a failure point at that core's failing offset. A larger subset that failed a group is a combination; if no tested group failed it ends `fallback` with a combination over all remaining candidates at their failing offsets. Before a combination ends, unless the profile already breaks it, member probes find how shallow each member must be, one member at a time in core-id order. A probe is a group of stage `probe` with the probed member at the probe offset, members already probed held at their shallowest failing offsets, the rest of the combination at their failing offsets and every other core at its parked offset, run at the duration of the hunt's last group. For the probed member, its failing offset is the deepest known failure and its parked offset the shallowest known pass. Probes go 1, 2, 4, … counts shallower than the failing offset until one passes or would reach the known pass, then bisect between the shallowest failure and the deepest pass until they are one count apart; a skipped probe stops probing. The hunt then ends `combination`, recording each member's shallowest failing offset as the `hunt.end` `members`; the combination sits at those offsets and the combination backoff rule picks the member to move. An attributed failure during a group ends `direct` and records a failure point at its actual applied offset, even on a core held at parked offsets. `hunt.end` precedes its `backoff` or `combination`; a combination already broken by the profile needs no further backoff. Resume uses cause linkage to emit each missing commitment once. A reset cancels an open hunt and requeues its failure.

## Deepening

Deepening starts only after search, hunts, reruns and the open cycle finish, when a passed full-cycle profile exists and a core is not at its limit or a globally deeper total is reachable. When only the open cycle stands in the way and an earlier clean cycle, run with every core at its limit on a profile at least as deep as the current one, has no failure contradicting it since, checking ends an incomplete open cycle without passing instead of running its remaining work; a fully executed cycle closes passed with its normal full-cycle coverage before deepening starts; the non-passing end's reason names the covering cycle and its cause cites that cycle's end. Deepening targets the safe profile with greatest total depth over all cores, with preferred-core ranking and then core-id order breaking ties. `deepening.round` snapshots target, the newest passed full-cycle profile as its base, and proposed profile. Cores that must become shallower yield first; those moving deeper go halfway toward their target. Each move is a decision; after the round's last move, whether each core is at its limit is re-evaluated and one `profile.change` applies them all.

Only deepened cores need checks: each runs `n` R1 trials and `n` R2 trials at `short_trial_s` alone, with the round's index freezing those catalog workloads. For every R7 workload, the round checks the partial where a deepened core belongs to the top group under the current request order, and its CCD's full part when that core is or becomes a top requester there; each selected part needs `n` trials at `short_trial_s`. Checks shared by several deepened cores run once. Request order uses the proposed profile, so a rank change selects the new part and the next checking chain derivation re-orders. Every multi-core R7 failure requires voltage-targeted backoff or a zero-offset dead end, never a hunt. A failure ends the round before attribution or backoff, and a passed round records its end; resume completes missing moves and checks without duplicating decisions. A new failure point or combination that makes the proposed profile unsafe ends the round without applying it.

Carried passes and failures count across the deepening-round boundary for these checks, under the same class, profile and invalidation rules. A round accepted using carried evidence cites its carried sequences and source sessions.

## Defect list

Known defects identify decisions made under earlier builds whose decisions cannot safely be rerun. On resume, their journal causes and build fixes stamps are matched as specified in `journal.md`. A too-cautious finding leaves checking running and `status` names the affected cores' individual reset commands. A too-aggressive finding stops unattended tuning until an operator has answered a terminal reset prompt. Resetting a core clears its failure point and the combinations naming it, then searches from its baseline.

## Reset

- `reset --core N`: records `command.reset` and queues the reset. The next `run`, after pending attribution, cancels an open hunt or deepening round and ends an open cycle without passing, then records `core.phase` to `search` at the baseline clamped to [-50, 0]. It clears the core's failure point and pass and every combination that includes it (listed in `cleared_combination`). A failure point at 0 is cleared too.
- `reset --all`: archives the session (`journal.md`). The next `run` starts a new session with a fresh baseline and BIOS context, carrying nothing from the archived one.

`reset` writes to the journal, so it refuses while a `run` holds it (`journal.md`).

## Dead ends

togi stops when it cannot make progress:

| Condition | Why it cannot continue |
|---|---|
| Attributed failure at offset 0, or an unattributed together failure with every core at 0 | The instability is not caused by Curve Optimizer. |
| SMU readback differs from the written value, or an SMU command fails | Offsets can no longer be trusted. |
| The same backend has `dead_ends.inconclusive_in_a_row` consecutive inconclusive trials (3 by default), then three more consecutive inconclusive trials after waits of 60, 300 and 1800 seconds before those trials, or is missing | No evidence can be produced; the `no_evidence` dead end fires at `dead_ends.inconclusive_in_a_row + 3` (6 by default). |
| 3 stray crashes in a row | The machine crashes before togi acts: a boot loop. |
| A backend thread observed outside its allowed logical CPUs | Attribution is broken. |
| Preflight fails | The environment is not the one being tuned. |
| An unanswered too-aggressive defect without a terminal | Earlier decisions may have moved offsets deeper than proven; an operator must decide whether to reset the affected cores. |
| Thermal-trip reset without higher-precedence failure evidence | Cooling must be checked before tuning again. |

What a dead end does in each run mode is in `runtime.md`. Thresholds are configurable.

A dead end follows from evidence recorded in the journal, not from memory, so a kill between the evidence and the `deadend` event still stops the next `run` before any SMU write. The evidence:
- an `smu.error`, or an `smu.readback` whose offset differs from `expected`;
- a `trial.end` with `escaped` CPUs;
- a backend's streak of inconclusive `trial.end`s reaching `dead_ends.inconclusive_in_a_row + 3`, after waits of 60, 300 and 1800 seconds before the three trials following the initial `dead_ends.inconclusive_in_a_row` consecutive inconclusive trials; trials interrupted by a stop or restart do not count;
- the stray-crash streak reaching the threshold.

A `deadend` consumes the evidence it reports: the SMU flag, escape flag, thermal trip, inconclusive streak or stray streak. Other conditions are evaluated fresh by the following `run`. Preflight repeats every run. A failure at 0 leaves failure point 0 on its core, so every later run stops until reset.

That fresh evaluation applies only after the dead end has recorded its boot action and `shutdown`. If a process stops between `deadend` and those events, the next `run` finishes that same dead-end action and exits without making a tuning decision.

