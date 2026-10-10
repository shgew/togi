# Tuner

Normative rules for how togi moves offsets. Terms are defined in `GLOSSARY.md`. Regimes, workloads and failure detection are in `workloads.md`; how decisions are recorded is in `journal.md`.

## Ruleset

The ruleset is the hardcoded strategy: search strides, offset range, phases, evidence, failure-point and combination rules, hunt, the two phases, and checking coverage. Changing these bumps `tuner.Ruleset` and archives an active older session into a seeded new session ([ADR 0019](../adr/0019-a-ruleset-change-starts-a-seeded-session.md), [ADR 0020](../adr/0020-hunt-and-refine.md), [ADR 0038](../adr/0038-self-sufficient-cores.md), [ADR 0040](../adr/0040-located-hunts.md), [ADR 0024](../adr/0024-schedule-from-uncontradicted-evidence.md), [ADR 0027](../adr/0027-carry-trial-facts.md)). Changes to configurable defaults and fixes that record facts more accurately do not bump it.

The evidence epoch (`tuner.EvidenceEpoch`) separately versions compatibility of trial outcomes: workload content, the backend configuration togi generates, intended durations, and pass/failure detection (`workloads.md`). `session.start.evidence` records it. Without that field, a session with ruleset ≥ 6 has epoch 1; an older session has epoch 0. A transition drops passes from other epochs but keeps eligible failures. An epoch change is not a ruleset or journal-schema bump. A new backend build needs no epoch change: its backend identity changes (Evidence).

Transitions record eligible same-BIOS trial and idle-failure facts before `session.carried` (`journal.md`, Transitions). The tuner treats carried legacy `record_only` R7 outcomes (written under ruleset 8) as ordinary evidence, with no aging; it does not carry profile offsets or clean cycles. Carried multi-core R7 failures are attributed and evaluated before the first trial.

Multi-core R7 means whole-CCD, partial and all-core loads. R1–R6 evidence and search, R6 idle failures, single-core failures and their hunts, failure points and combinations from those paths, and their first-failure rules are separate from it. The tuner charges an unattributed multi-core R7 failure to the loaded cores by voltage-targeted backoff and locates it on its unloaded cores only when that stops helping ([Escalation to a located hunt](#escalation-to-a-located-hunt), [Hunt](#hunt)), and reruns a failing trial with every core at CO 0 before any `failure_at_zero` dead end ([Dead ends](#dead-ends)).

## Invariants

1. Every offset stays within [-50, 0]. togi never writes a positive offset.
2. During a trial run alone only the target carries a nonzero offset. Every other core is written to 0 first, including at the start of each boot, when firmware has restored the BIOS values.
3. Every SMU write is preceded by a durable intent event and followed by a readback. A readback that differs from the written value is a dead end.
4. New tuning-profile applications and restoration do not deepen into a recorded failure point or combination. Reset removes the constraints it clears.
5. Offsets get deeper only in search and in phase-2 rounds.
6. Every decision is an event that names its cause (`journal.md`).

Invariant 4 concerns recorded constraints, not a guarantee that a trial will pass. A failure can make the profile still applied on the hardware reach a newly learned constraint until its backoff is applied. Hunts deliberately revisit failing offsets before committing their failure points or combinations; their experimental parked profiles are not new tuning-profile choices. A group reaching a newly recorded constraint is skipped ([Hunt](#hunt)). The application path writes shallower offsets first and checks each deeper write against the recorded constraints, including when applying a parked profile or restoring offsets. An all-zero rerun is diagnostic, not a replacement tuning profile ([Dead ends](#dead-ends)).

## Session start

1. Preflight (`runtime.md`) passes, or togi stops at a dead end.
2. The first `run` of a session reads every core's offset from the SMU as the baseline and records the BIOS context.
3. Each core's start offset is the configured override if one exists, else its baseline, clamped to [-50, 0]. A configured candidate solo limit starts the core in search at that offset with `check_solo_limit` and frozen R1/R2 workloads; it still needs the full evidence rule.

   A session started by a transition (`journal.md`, Transitions) seeds cores from `session.carried`, citing that event after the baseline. Precedence: configured candidate solo limit, configured start offset, carried candidate solo limit, baseline. A carried candidate solo limit starts search with `check_solo_limit` and reason `candidate solo limit <candidate_solo_limit> carried from session <id>`. A carried failure point is recorded in the first `core.phase`; a start at or deeper than it is clamped one count shallower, regardless of its source. A failure point at 0 remains a dead end until reset.
4. A nonzero baseline produces a notice recommending BIOS CO 0 for tuning. togi still starts from it.
5. A later `run` whose BIOS context differs archives the old session and starts a new seeded session carrying candidate solo limits but not failure points and combinations (`journal.md`, Transitions).

## Scheduling

Cores are visited in CCD-alternating order: 0, 8, 1, 9, ... 7, 15. Each turn goes to the next core still in search. Its R2 trial follows a passing R1 in the same turn. An inconclusive trial retries the same class unless a reset supersedes it or the requirement it retried has changed. A saved retry belongs to one context: its search turn, phase-2 round, checking cycle step, hunt group, rerun obligation, or all-zero rerun's source failure, and its class. Only that context and class serve it. It is dropped when its context ends (the core decided, the round, hunt or cycle closed, the hunt moved on to another group, the obligation retired, the source failure invalidated), when a reset supersedes it, or when a resume's `config.loaded` changes its required class, count, window or requirement; the current requirement runs instead. Interleaving cores lets each cool between its own trials.

## Search

A search step runs one R1 trial alone then one R2 trial alone at the same offset, 90 s each by default. Its first failure rejects it. The eventual candidate solo limit additionally needs `n` passes in each of its frozen R1 and R2 trial classes, using `durations.search_trial_s`.

Carried passes can satisfy `check_solo_limit` in its frozen R1/R2 classes at `durations.search_trial_s`, even though they precede the solo limit-check phase boundary. Ordinary search steps still require live trials.

The candidate check's live-pass window starts at `check_solo_limit`, not at the preceding search step. That step's own R1/R2 passes do not count, even if their workload and duration match the frozen classes. Without eligible carried passes, five additional launches per class are needed at the defaults. The check freezes workloads independently of the search step's workload rotation; a different workload is a different class.

Illustrative search for one core, starting at 0 with no failure point:

| Offset | Result | Next action |
|---|---|---|
| 0, -5, -10, -15 | Each step passes one R1 and one R2 trial | Keep the deepest passed step and descend by five |
| -20 | R1 fails; no R2 is launched | Record failure point -20; deepest pass is -15; try -16 |
| -16, -17, -18 | Each step passes R1 and R2 | Descend by one |
| -19 | The step passes R1 and R2 | One deeper count reaches -20; record `check_solo_limit` at -19 |
| -19 | Five new passes in each frozen R1/R2 class | Record the checked solo limit -19 as `pass`, and phase `has_room` with offset -18, one count shallower (the margin) |

If that candidate check instead fails at -19, its stored pass at -19 is contradicted and discarded. With no retained pass, search backs off five counts to -14, then proceeds one count at a time after passing there, since failure point -19 is now known. Reaching a candidate check is not accepting the candidate.

When the candidate check passes, the core leaves search one count shallower than its solo limit, the **margin**: `core.phase` records `offset` as `min(solo limit + 1, 0)` and `pass` as the solo limit, and its reason says `checking starts at X, one count shallower than the solo limit Y (margin)`. Whether the core has room or is at its limit is evaluated at the margin offset: it is at its limit when one count deeper than that offset reaches a failure point or combination, or the floor. In the example the margin offset -18 has room, because -19 reaches no recorded constraint. A solo limit of 0 has offset 0, no margin, and its core is never a phase-2 candidate. The margin is a method constant of one count ([#493](https://github.com/shgew/togi/issues/493), Q31 and Q1), not fitted to any machine. The tuner keeps each core's solo limit from that `core.phase`, not from later `pass` values, which a failure can discard; a reset clears it ([Phase 1](#phase-1)).

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

All conclusive trials and facts enter the evidence rules, including former `record_only` R7 outcomes. Every multi-core R7 failure follows [R7 voltage-targeted backoff](#r7-voltage-targeted-backoff); other trials retain the first-failure rules.

One trial is one workload launch, with no internal relaunch. The pass rule is `n = ceil(ln(evidence.miss) / log1p(-evidence.rate))` passing trials, five at the defaults (0.05, 0.5). Outside multi-core R7 the first failure rejects a step and restarts the consecutive pass count. The full count applies to a candidate solo limit's R1 and R2 classes, every hunt group and every phase-2 round check.

`evidence.rate` is the per-trial failure probability the replicated pass rule is meant to detect. `evidence.miss` is the allowed probability of seeing only passes despite that rate. Assuming independent trials with a fixed failure probability at least `rate`, the probability of `n` passes is at most `(1 - rate)^n`; the rule chooses the smallest integer `n` that makes it no greater than `miss`. With rate 0.5 and miss 0.05, four passes leave probability 0.0625 and five leave 0.03125. These are assumptions about repeated trials, not a measured hardware failure rate or a bound on future use.

The defaults are heuristic choices. [ADR 0020](../adr/0020-hunt-and-refine.md) introduced this rule but records no empirical calibration for 0.5 or 0.05. [Issue #493](https://github.com/shgew/togi/issues/493) notes that the existing first-failure/five-pass rule already matches the sequential probability ratio test (SPRT) bound at those values; this equivalence does not justify the chosen values.

A trial class is `(regime, workload, sorted loaded cores, duration_s)`, using intended duration, not the elapsed time before a crash. The ledger records each conclusive trial's class, sequence, applied `trial.intent.profile` and outcome. A pass at profile Q counts toward P only when Q is at least as deep as P and follows the latest failure of that class at a profile at least as shallow as P; a failure at Q rules out P when Q is at least as shallow as P. Requirements on the same class within a step add, so a trial counts once. An idle crash has wildcard R6 class with all cores loaded and invalidates all such R6 classes. Passes at deeper profiles can survive backoff; deeper moves need new evidence.

Before folding a `trial.intent` into its running state, the tuner stores the requirement the trial was scheduled for: kind, class, evidence-window start and rule, needed count, checking step and part. Scheduler builders attach that same value to their trial. The requirement stays with the trial for the session, without additional journal fields; projections and historical evidence counts read it rather than reconstructing an obligation from later scheduling state. A trial closed after a crash retains its intent-time requirement even when resume loads changed configuration. An all-zero rerun's requirement is one conclusive trial whose window opens at the failure it cites, so passes of its class never advance its number. A hunt or deepening trial takes its requirement only from the open hunt or round that its `hunt` or `round` names; an intent naming another has none. The trial number a projection shows never exceeds the class's needed count. From `crash.detected` until recovery closes the crashed trial with its `trial.end`, that trial stays in flight: the dashboard keeps its cycle part, hunt group or search turn marked running, while the forecast already shows what follows the crash.

Passes are keyed by backend identity: the package store path the latest `config.loaded` records for the backend that runs the class's workload (`config.backends.mprime` or `config.backends.ycruncher`). The ledger records each pass with the identity in force when its `trial.end` or `trial.carried` is folded, and a pass counts only while that backend's recorded path is unchanged. This holds wherever passes count: step and cycle requirements, the `n` newer passes that outweigh a failure, monotonicity warnings, the checking exposure, R7 voltage targets and self-sufficiency. A `config.loaded` that records a new path for one backend, for example after `nix flake update nixpkgs` between boots, restarts the pass counts of that backend's classes, live and carried alike; the other backend keeps its passes, and recording the earlier path again restores them. Failures are not keyed: a failure under one build still rules out its profiles and needs `n` newer passes under the current build, since a crash is evidence about the hardware whichever binary loaded it. Recorded decisions, passed cycles and clean cycles stand; an open checking cycle needs new passes in the restarted classes. A workload outside the catalog has no backend and an empty identity.

The ledger folds `trial.carried` and `failure.carried` before live evidence, ordered by their sequence in the new journal and marked as carried. Carried passes count across the local sequence boundaries for candidate-solo-limit checks, hunt planning and outcomes (including projected pass counts), rerun obligations and phase-2 round checks. Full-cycle coverage uses only live passes since its cycle start. Carried failures count everywhere failures count, regardless of those local boundaries; multi-core R7 failures are processed once for voltage-targeted backoff. Failures invalidate earlier covered passes and keep a class failing until `n` newer covering passes; outside multi-core R7 they also participate in monotonicity warnings. A core reset discards carried evidence under the same loaded-core rules as live evidence.

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

The fixed 3.6 mV per count is a modeling assumption, not a measured universal CO-to-voltage conversion. It entered `internal/requests` in [commit c9cbf3ae](https://github.com/shgew/togi/commit/c9cbf3ae) without a calibration source. Both the request shifts above and the count conversion in [R7 voltage-targeted backoff](#r7-voltage-targeted-backoff) use it. The simulator's request-slope fit also uses 3.6 mV per count as a prior (`tools/fit/voltage.go`), so that fit is not independent validation of the fixed tuner constant.

A multi-core R7 failure naming a core through a backend signal or exactly one core-local MCE counts against that core. An unattributed failure counts against the loaded CCD's top group, or each CCD's top group for all-core loads. When `stalled_core` identifies one CCD, only that CCD's top group is affected; it does not name the failing core. A live unattributed failure is charged under these rules directly. It is first located by a located hunt ([Hunt](#hunt)) only when it escalates: its load already has two voltage-targeted backoffs since the load's latest passing trial, or every loaded core on its affected CCDs is at CO 0 while an unloaded core is not ([R7 voltage-targeted backoff](#r7-voltage-targeted-backoff), [ADR 0053](../adr/0053-escalation-only-located-hunts.md)). A live or carried failure whose loaded cores were all at CO 0 while an unloaded core was not is also located, unless it names a core off CO 0. Only a `loaded` end charges a located failure under these rules, through the hunt's group failure instead when that failure named a loaded core. Otherwise multi-core R7 never starts a hunt, parked group, member probe or combination. These rules apply to live and carried failures and to any already-recorded known-failure skip, without duplicating an observation.

An unattributed failure whose affected top group is entirely at CO 0 steps down the same CCD's request order to the next group with a movable loaded core. When an all-core failure affects several CCDs, a CCD whose loaded cores were all at CO 0 in that trial is not affected while another affected CCD has a movable loaded core; the failure counts only against the CCDs that do. It dead-ends with `failure_at_zero` only when no affected CCD has a movable loaded core: irrespective of idle cores once its located hunt ended `loaded`, or when it was not located; a `stalled_core` CCD at 0 dead-ends irrespective of offsets on another CCD under the same condition. A named core at 0 dead-ends only if it was in its own CCD's top group in that trial; otherwise route the failure to that CCD's top group, including the same stepdown rule. A named core at 0 on a CCD with no loaded core in that trial has no top group of its own: the failure counts as unattributed, against the loaded CCD's top group. Each of these dead ends first needs the failing trial's all-zero rerun ([Dead ends](#dead-ends)); a failed locate whose loaded cores were all at CO 0 already was that rerun. Once the all-zero rerun passed, the failure no longer counts against its named core or its `stalled_core` CCD: it counts as unattributed, under the stepdown rules above, against the CCDs with a movable loaded core, and its decisions cite the rerun's `trial.end`.

The following ordered table summarizes the multi-core R7 failure path. Attribution uses the failed trial's profile and request order, not later measurements. Rows that say to continue return to the remaining rows with the updated attribution.

| Order | Condition | Action |
|---|---|---|
| 1 | The failure meets the located-hunt rules in [Hunt](#hunt), including its escalation, and would otherwise require a move or dead end. A live unattributed failure that does not escalate skips to row 4 or 5 | Locate it before charging the loaded cores. No loaded core moves for it while the hunt is open |
| 2 | That hunt ends `culprit`, `direct`, `combination` or `fallback` | Consume the original R7 failure through the hunt commitment and queue its rerun obligations; do not charge the loaded cores again |
| 3 | That hunt ends `loaded` | Continue with R7 attribution. If its group failure named a loaded core, charge that group failure against that core at its own applied offset, rather than also moving a core for the original failure |
| 4 | A backend signal or exactly one core-local MCE names a core | Charge that core. A named CO-0 core that was not a top requester routes to its CCD's top group; if its CCD had no loaded core, route as unattributed |
| 5 | No core is named | Charge the affected CCDs' top groups. `stalled_core` restricts the affected CCD, not the culprit within it. If several CCDs are affected and any has a movable loaded core, omit affected CCDs whose loaded cores were all at 0 |
| 6 | A charged top group has no movable core | Step down that CCD's request order to the first group with a movable loaded core |
| 7 | No affected CCD has a movable loaded core, or a named CO-0 core was a top requester | Test the zero-offset claim under [Dead ends](#dead-ends). A `stalled_core` restriction is not widened merely because another CCD has a nonzero offset |
| 8 | The failed profile was already all-zero, or its all-zero rerun fails | Record `failure_at_zero`. A failed locate with every loaded core at 0 already supplies the all-zero rerun |
| 9 | The all-zero rerun passes | Do not dead-end. Drop both named-core and `stalled_core` restrictions and route as unattributed against CCDs with movable loaded cores, including a located hunt if required |
| 10 | A movable core is selected, but its current offset is already shallower than its offset in that failed trial | No move now. Keep the failure pending in case that core returns to the failing offset or deeper; this alone does not end a phase-2 round |
| 11 | A move is still required | End an open phase-2 round first. Apply [R7 voltage-targeted backoff](#r7-voltage-targeted-backoff), record the moved core's failure point, and satisfy [Checking](#checking)'s rerun obligations before resuming the open cycle |

## R7 self-sufficiency evidence

Self-sufficiency is counted per `(core, R7 workload)`, across durations and loaded sets. It counts the R7 passes in the evidence ledger, live and carried, under the backend's current identity ([Evidence](#evidence)) where the core was a top requester at an offset equal to or deeper than its current offset; a reset of the core removes the evidence involving it. A core is self-sufficient for a workload once it counts one such pass. Failures do not enter this count. `status` and the dashboard report it ([runtime.md](runtime.md#commands)); no tuner decision reads it, and it never tolerates a failed trial, which follows [R7 voltage-targeted backoff](#r7-voltage-targeted-backoff). One clean cycle still concludes a requested one-cycle run, without a bound on future stability.

## R7 voltage-targeted backoff

Every multi-core R7 failure that is not waiting for, or consumed by, its located hunt ([Hunt](#hunt)), because it does not escalate ([Escalation](#escalation-to-a-located-hunt)), requires a move unless the zero-offset rules above establish a dead end or the core the move would raise, the named core or the core selected below, already sits shallower than its applied offset in the failed trial. Such a failure needs no move while that holds: it records no decision and stays pending, taking effect again if that core returns to its failing offset or deeper, and it never ends a phase-2 round. Choose a target V* from profiles with `n` passing trials, where n is the Evidence pass count:

- For an unattributed failure, select the first request group on each affected CCD containing a movable core. Move one movable core in that group, the lowest-ranked by preferred-core ranking, then core-id order. Use the lowest top request above the failing top request among passing profiles of the same workload and loaded set. In a whole-CCD or partial load, use that CCD's numbers; in all-core loads, choose independently for each affected CCD. A named core at 0 that was not a top requester uses this path on its own CCD.
- For a named core X, use the lowest top request X has passed under in any load of that workload containing X, with `n` passes on that profile. Its CCD's passing median clock must be at least the failing trial's `ccd_mhz`; omit the clock filter when the failing trial has no clock telemetry.

The moved core's own failing request r comes from its trial's `voltage_requests_v`, or from request-order steps 1 and 2 on its applied profile. Raise it by `max(1, ceil((V* - r) / 0.0036))` counts; without a qualifying passing target, use one count. When stepping down past a CO-0 top group, also require at least `floor((r_top - r) / 0.0036) + 1` counts to put its estimated request above the failing top request. Take the larger count and move that many counts shallower than the core's applied offset in the failed trial, and at least one count shallower than its failure point, then clamp the offset to [-50, 0]; the clamp can prevent reaching that voltage. When step 3's offset proxy stands in for r, even beside other cores' measurements, the move is one count: no V*, stepdown minimum or pass applies, and stepdown follows offset order without claiming that its physical request exceeds the top. The decision cites the failure, request measurements and passes defining V*, and explains the voltages, count and any offset-order fallback without printing invented voltages.

Record the moved core's failure point at its applied offset in the failed trial, keeping the shallowest point. Phase 2 cannot return there; ordinary backoff rerun obligations follow.

### Escalation to a located hunt

A live unattributed multi-core R7 failure whose failing profile has an unloaded core off CO 0 escalates to a located hunt ([Hunt](#hunt)) only when stepping back busy cores stops helping:

- Its load, the R7 workload and the sorted loaded cores whatever the duration, already has two voltage-targeted backoffs since its latest passing trial before the failure.
- Or every loaded core on the failure's affected CCDs is at CO 0 while an unloaded core is not: no loaded core can step back.

Otherwise it takes the voltage-targeted backoff above, as in ruleset 9, including the step-down past CO-0 groups. The all-zero rerun before any `failure_at_zero` dead end is unchanged ([Dead ends](#dead-ends)).

The count is derived from the journal, so replay and resume reproduce it. A backoff counts toward its load when its cause names an unattributed multi-core R7 failure of that load: one naming no core, or one routed as unattributed under the rules above, a named CO-0 core on a CCD with no loaded core or a failure whose all-zero rerun passed. A failure that moves one core on each CCD counts once. A backoff for a failure attributed to its named core, charged to that core or routed to that core's CCD's top group, never counts, nor does the located-hunt backoff that charges a named core. Any recorded passing `trial.end` or `trial.carried` of the load restarts the count, and a pass of another load, workload or loaded set does not. Each load counts separately.

Phase-2 blames and confirmation reverts are `deepening`-phase decisions: they answer failures of deepened cores, not unattributed voltage-targeted backoffs, so they never count toward a load's escalation ([Phase 2](#phase-2), [ADR 0054](../adr/0054-two-phases-replace-the-deepening-loop.md)).

A held later failure ([Later failures](#later-failures)) moved no voltage, so it never counts toward escalation; the step-back a second failure causes counts as any backoff does.

Each such backoff's reason says which backoff of its load it is since the last pass and that a located hunt follows only after two. A located hunt's `hunt.start` reason says why the failure was located: it cites the backoffs that escalated it, or says that every loaded core of the affected CCDs is at CO 0 while an unloaded core is not.

## Failure points and combinations

A failure point is reached when `P[c] <= fail[c]`; a combination is reached when every member `m` has `P[m] <= C[m]`. A core is at its limit at -50 or if one count deeper would reach a failure point or combination. Re-evaluate whether each core is at its limit after every failure point or combination or offset change. Failure points and combinations accumulate until reset.

These constraints select the tuning profile and bound deeper writes ([Invariants](#invariants)). They do not declare every experimental profile stable. A hunt can reproduce a failure before recording its constraint; after that commitment, it cannot launch a group that reaches the newly recorded constraint.

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

R6 loads every core. Each R7 checking step runs each CCD's full part first, then its partial chain, in CCD order; the all-core part runs last. On one CCD the full part is the all-core part and runs only once, before its chain. Each partial idles the predecessor's top requesters, including ties within 1 mV, leaving at least two loaded cores. Every full, partial and all-core part has three short trials at `short_trial_s` and one long trial (counts add when durations coincide). On one CCD every long trial runs `checking_all_core_s`; on `n` CCDs each full and partial part's long trial runs `floor(checking_all_core_s / 4)` and the all-core part's runs `checking_all_core_s - n * floor(checking_all_core_s / 4)` ([workloads.md](workloads.md#durations-and-checking-schedule)). Each trial has its own intent, instance set and teardown. Only loaded cores run backend instances; the profile offsets on all cores stay applied.

`checking.step` records the R7 occurrence's starting profile. Once the predecessor has fulfilled its passing-trial requirements, `checking.chain` derives the next partial from that load's current request order, citing the measurement sequences or explaining the offset fallback. It records the tie groups, next loaded set and derivation profile; an empty set records that removing the top group would leave fewer than two cores. After a profile change inside the cycle, every derived part re-derives from its predecessor, from the full part down, before its step counts, whether or not it has run. A part whose loaded set is unchanged keeps its passes as ordinary evidence and needs no new trial; a changed set is a new trial class and needs its own passes; a set no longer in the chain is no longer required. A cycle passes only when every chain was derived at its ending profile, so the ending profile's riskiest load has run ([#489](https://github.com/shgew/togi/issues/489), [ADR 0055](../adr/0055-rederive-r7-chains-after-a-backoff.md)). Partial outcomes are ordinary evidence for cycle requirements, full-cycle coverage, reruns, phase-2 round checks and the R7 self-sufficiency ledger. Historical ruleset-8 `record_only` outcomes, live or carried, are ordinary evidence too; the tuner emits no such marker.

## Crashes

A crash is classified by what its boot recorded. A together application counts as applied from its first nonzero `smu.intent`, before `profile.applied`. A hunt group's parked application likewise counts as parked from its first nonzero `smu.intent`, before `profile.applied` or `trial.intent`; an attributed idle failure during it ends the hunt `direct`, naming that core at its actual applied offset. A backend signal recorded in `trial.progress`, a trial-window MCE selected by boot-local monotonic time or an uncorrected MCE in the next boot takes precedence over reset reason. Otherwise a thermal trip is a dead end; a power-button reset during a trial is its failure, but without an open trial is inconclusive. When reason reporting is supported and confirmed, no reason line means power loss and is inconclusive. An unknown or unsupported reset reason retains the usual classification:
- A trial in flight: a failure of that trial. Alone failures belong to the target; together and parked attribution follows Checking.
- An applied profile with no trial in flight: an idle crash, treated as an unattributed R6 failure with all cores loaded; a trial alone all-zero profile changes nothing.
- Nothing applied: a stray crash. Reaching `dead_ends.stray_crashes_in_a_row` is the boot-loop dead end.

Restoring offsets before `shutdown` (`runtime.md`) is not an application: its `smu.intent` events cite `session.baseline` and leave the boot's last application as it was, so a crash part-way through is classified by what was applied before. After `profile.restored` the cores are back at togi-independent values, and a crash counts as if nothing was applied.

`crash.detected` carries the condition of the boot's last application. Its `inconclusive` flag reflects the evidence precedence above: a thermal-trip reset with higher-precedence failure evidence is not inconclusive and is processed as the usual trial or idle failure, not a thermal dead end.


A crash with a trial in flight and an idle crash are failures like any other: no rule in a running session counts them against search or checking, caps them, or pauses after a run of them. They count as a cost when tuner rules are chosen and gated: the median crash count is a criterion of the Ruleset 11 gate ([ADR 0052](../adr/0052-crashes-are-a-cost.md)). Only stray crashes are counted, for the boot-loop dead end.

## Decision events

Moves are `tuner.decision`: `step_deeper`, `check_solo_limit`, `deepen`, `yield` and `backoff`, with phases `search`, `checking`, `hunt` or `deepening`. The `deepening` phase marks phase-2 round moves, yields and blames and the confirmation cycle's reverts ([Phase 2](#phase-2)). `check_solo_limit` freezes the R1/R2 workloads; `core.phase` to `has_room` or `at_limit` records the checking start offset (the margin offset), the checked solo limit as `pass` and the failure point ([Search](#search)). Each decision cites its cause and reason. A search backoff says `failed`; other backoffs say `backed off`. A phase change after an offset or failure point or combination update re-evaluates whether the core is at its limit.

A `checking` `backoff` with equal `from_offset` and `to_offset` answers either a failure at a deeper offset than the core's current one, whose reason ends `already shallower`, or holds a later failure ([Later failures](#later-failures)), whose reason starts `holds`. The two differ structurally: a hold answers a failure that applied the core's current offset.

## Checking

Checking begins when no core remains in search. Its first `profile.change` has `from: null`. A profile change does not end an open cycle; passing steps on a deeper profile remain evidence after a backoff, and the cycle's R7 chains re-derive at the new profile as described in [Together trial sequence](#together-trial-sequence). Only `reset --core` ends a cycle without passing.

Failures, crashes, hunts and backoffs inside a cycle do not end it or restart its evidence window. A passed cycle can contain dozens of failures and backoffs: “passed” means that its requirements eventually passed on profiles that still cover the ending profile, not that every trial passed on the first attempt.

A cycle captures the configured schedule in its start event. R1 and R2 occurrences select successive catalog workloads (three occurrences cover each catalog); R3, R4 and R5 run on each core; R6 runs all cores. Each R7 occurrence selects its next R2 workload and requires every full, derived partial and all-core part's three short and one long passing trials. Requirements sharing a trial class add, including repeated partial classes, and trials passed on sufficiently deep profiles since the cycle start can fulfill them. The first unmet requirement of the first unmet step runs next; a partial is not derived until its predecessor passes. The cycle ends passed only after every chain is complete and every requirement passes, and is full when it also has at least three R1, R2 and R7 steps and one each of R3, R4, R5 and R6. A non-full end records the missing coverage. The newest passed full-cycle profile that is shallower than the failing profile on at least one core supplies a hunt's parked offsets.

Only live passes since the cycle start fulfill cycle requirements; carried passes never fulfill them. Failure invalidation is global, including carried failures before that start, so the live-pass boundary does not restore contradicted evidence.

Outside multi-core R7, together and parked attribution uses the backend instance's reported core first, then exactly one core named by core-local MCEs, then the single nonzero core in the applied profile. An attributed failure records the failure point at the applied offset and backs off that core to at least one count shallower, including when it was held at a parked offset. A failure at 0 is a `failure_at_zero` dead end once the failing trial's all-zero rerun failed too ([Dead ends](#dead-ends)); if that rerun passes, the failure no longer names that core: a together failure is hunted as unattributed, and a parked failure is the group outcome. If the core was already shallower, its offset need not move. An unattributed together or idle failure queues a hunt of the failing profile and trial class instead of backing off the loaded set. An unattributed parked failure is the group outcome. Multi-core R7 instead follows R7 request order, attribution and voltage-targeted backoff above. Inconclusive trials retry the same class.

After phase 2 concludes, a failure may be held instead of answered by the rules in this paragraph; [Later failures](#later-failures) owns which ones and what a hold records.

A together checking requirement outside multi-core R7 with a valid known failure goes directly to that failure's attribution or hunt rather than starting the workload. Attributed skips retain the known failing offset for the failure point and back off beyond it; unattributed skips hunt the known full failing profile and class. The decision changes pending state exactly as a live failure would, so a skipped requirement cannot repeatedly skip without making progress. The same pre-scheduling rule applies to reruns and phase-2 round checks outside multi-core R7; a failed round check closes its round before the backoff or hunt ([Phase 2](#phase-2)). Multi-core R7 never skips a trial for a known failure.

When a backoff or hunt commitment changes an offset, checking first reruns the failed class: `n` trials at `short_trial_s`, then one at its original duration if different. These obligations are FIFO. A rerun's evidence window starts at the failure it answers: the failure the commitment cites, or, for a hunt commitment, the hunt's source failure (`hunt.start.failure`), not the backoff or commitment itself. Passes of the class recorded after that failure at the same or deeper offsets than the current profile on every core can answer the rerun, including passes recorded between the failure and the backoff, so a rerun may need fewer new trials or none. A rerun cites the newest queued failure of its class; this does not change the obligations or their evidence windows.

Carried passes can satisfy a rerun's class requirements despite preceding its obligation boundary; failures, including carried ones, retain their normal invalidation effect. When carried passes answer the rerun, the following checking-cycle or phase-2 round decision cites those facts and names their source sessions. A rerun does not supply carried passes to full-cycle coverage.

Rerun obligations and cycle requirements read the same evidence ledger with different windows. A live rerun pass after the open cycle's start can also fulfill that cycle's requirement when its class and applied profile cover it; the `rerun` flag and absence of a `cycle` field do not prevent this. The same pass is not counted twice within the cycle: repeated requirements of its class add. Carried passes can discharge a rerun but not the open cycle's requirements.

Illustrative journal sequence, not a recorded run: on two CCDs at the defaults, an R7 full or partial part has three 120 s passes and one 300 s pass to earn. Assume no eligible carried passes, no other pending reruns and no new failure during the rerun.

| Event | Effect |
|---|---|
| `checking.cycle` start | Open one cycle and capture its schedule |
| Three 120 s `trial.end` passes for the part | Meet its short requirement at profile P |
| Its 300 s `trial.end` failure; `failure`; `tuner.decision` backoff; `profile.change` to shallower Q | Keep the same cycle open. Queue the failed class's rerun |
| Five 120 s rerun passes at Q (`phase: checking`, `rerun: true`, no `cycle`) | Meet the short rerun obligation; also cover the part's three short cycle passes. Valid earlier passes at P can still cover Q |
| One 300 s rerun pass at Q | Meet the long rerun obligation and the part's long cycle requirement |
| Next checking action | Derive the next partial if this part is a chain predecessor, or run the first remaining unmet cycle requirement. If the backoff changed a chain's profile, every part of the chain re-derives first; a part whose loaded set is unchanged runs nothing new. Do not repeat this part merely because the answering trials were reruns |
| `checking.cycle` end, `passed: true` | All remaining requirements and chains have passed. The failure did not create a second cycle |

Other classes' earlier passes remain usable if they cover Q and have not been invalidated. If a failure or profile change leaves some class uncovered, only its unmet requirements need more trials. The five short and one long launches above are not a universal cost: eligible carried passes or live passes after the obligation's failure boundary can reduce it.

A clean cycle is a passed full cycle that ends at or after phase 2's conclusion ([Phase 2](#phase-2)). The phase-1 cycle never counts, and no earlier cycle is credited: a cycle is not ended early because an earlier one covers its profile. `run --cycles N` checks its stop rule before starting the next cycle and stops after N clean cycles: `--cycles 1` stops right after the confirmation cycle, and a larger N counts further cycles of indefinite checking. Without the flag checking continues indefinitely. `status` reports the clean cycles, their count and the latest one's number. A clean cycle establishes workload breadth, not a guarantee against rare failures or untested real use.

A cycle that passes before phase 2 concludes is therefore passed, not clean: it ends before the profile is confirmed, however few failures it contained. This is the usual distinction behind “passed, not clean”, not a count of failures within the cycle.

## Hunt

Hunts are limited to failures outside multi-core R7, including R6 idle and single-core failures, and to located hunts. The rules below retain their first-failure interpretation; R7's measured request attribution and voltage-targeted backoff do not apply to them.

A located hunt hunts a live unattributed multi-core R7 failure, including one naming a CO-0 core on a CCD with no loaded core, whose failing profile has an unloaded core off CO 0, that would otherwise move a loaded core or dead-end, and that escalates: its load has two voltage-targeted backoffs since its latest passing trial, or every loaded core on its affected CCDs is at CO 0 ([Escalation](#escalation-to-a-located-hunt), [ADR 0053](../adr/0053-escalation-only-located-hunts.md); [ADR 0040](../adr/0040-located-hunts.md) located every such failure first). It also hunts a live or carried failure whose loaded cores were all at CO 0 while an unloaded core was not, unless the failure names a core off CO 0: its locate holds every core at 0, so it is the failure's all-zero rerun ([Dead ends](#dead-ends)). A failure that names no core only because its all-zero rerun passed is located only when it escalates, or when its loaded cores were all at CO 0 as above; otherwise it takes the voltage-targeted backoff. When it is located, its `hunt.start` cites that rerun's `trial.end`. Other carried failures are charged as before. It starts once search is over; an open phase-2 round ends first, and no loaded core moves for the failure while the hunt is open. `hunt.start` records the failed load: loaded cores are held at their failing offsets and are never candidates, parked offsets are the failing profile with every unloaded core at 0, and candidates are the unloaded cores that were nonzero. Its first group, stage `locate`, has an empty subset and runs at the failed trial's duration; like any group it needs `n` passes and its first failure rejects it. A failed or skipped locate ends the hunt `loaded`. After a passed locate, the delta debugging below narrows the candidates unchanged, as if locate were not a group: a single candidate is the culprit at once. When that narrowing would end with no failed group, a group of stage `full` runs the full failing profile, every candidate and loaded core at its failing offset, at the failed trial's duration, even after a full group passed at `short_trial_s`, and never escalates beyond it. If it passes `n` trials, or is skipped, the hunt ends `loaded` instead of `fallback`, unless every loaded core was at CO 0: then it ends `fallback` like any hunt; if it fails, it is a failed group like any other, so an unattributed failure makes the candidates a combination that gets member probes. A group failure, locate included, naming a loaded core ends the hunt `loaded`, except after a passed locate whose loaded cores were all at CO 0; one naming an unloaded core at a nonzero offset ends `direct`; one naming any core at CO 0 after a passed all-zero locate, or an unloaded core at CO 0 otherwise, is an unattributed group failure. Every other end consumes the R7 failure, and its commitment's rerun obligation reruns the failed class. After a `loaded` end whose last group failure named a loaded core X, that group failure is charged as a named failure against X in its own failed trial: X's voltage-targeted backoff with its failure point at its applied offset in that trial, or, with X at CO 0, the zero-offset rules for a named core, whose dead end needs that group trial's own all-zero rerun unless it failed an all-zero locate. The decision cites the hunted failure, the hunt end and the group failure, and consumes the hunted failure, which moves no other core. After any other `loaded` end the failure follows R7 attribution and voltage-targeted backoff, which cite the hunt end and the trial that rejected its last group. A reset that cancels a located hunt leaves the failure to be located again.

Every unattributed together failure outside multi-core R7 is hunted unless its failing profile already reaches a recorded failure point or combination; then `hunt.skipped` explains why. An unattributed failure with every core at CO 0 has no candidate to hunt: it is the `failure_at_zero` dead end, naming no core. The hunt parks other cores at the newest passed full-cycle profile raised to the failing profile: each core takes the shallower of its passed full-cycle and failing offsets, so passes on the passed full-cycle profile cover parked offsets. A passed full-cycle profile that is nowhere shallower than the failing profile cannot supply parked offsets; the hunt falls back to older passed full-cycle profiles and finally all-zero. Candidates are precisely cores deeper at the failure than at their parked offsets.

After phase 2 concludes, a failure may be skipped as held instead of hunted, and a second failure loading one of the same cores within the window starts the hunt, citing both failures; [Later failures](#later-failures) owns the rule, its gate and its exceptions.

For each group, candidates in the selected subset take their failing offsets and every other core takes its parked offset. Delta debugging tests parts, then complements as granularity increases, retaining a failing subset. By default it starts at `short_trial_s`; only when the failed trial's duration is longer than `short_trial_s`, if all initial parts and complements pass, it tests the full failing profile, and if that too passes `n` trials, repeats at the failed trial's duration. Configuration still permits `search_trial_s` or `checking_trial_s` below `short_trial_s`; a shorter or equal failed duration does not trigger this full-profile check or duration repeat. At most one duration escalation occurs.

Partition order follows this pseudocode. `S` and each partition keep core-id order. When `hunt.start` is folded, the hunt snapshots the tuner's recent constraint cores; every partition uses that snapshot throughout the hunt, including planning and dashboard projections. Replay rebuilds the snapshot from the preceding events, without a new wire field. The optional repeated-core probe, duration prior, located-hunt endpoints and member probes retain the rules below and above.

```text
partition(S, g):
    g = clamp(g, 1, len(S))
    if g == 2 and S == original_candidates
       and S has both loaded and unloaded candidates:
        parts = [loaded candidates, unloaded candidates]
    else:
        parts = g contiguous pieces of S
        # Sizes differ by at most one; earlier pieces get the remainder.
    stably put pieces containing the hunt-start recent constraint cores first
    return parts

S = original_candidates; g = 2; stage = part; index = 0
duration = short duration, unless the longer-duration prior applies
run the locate group first for a located hunt
if an eligible repeated-core probe runs and passes, return to g = 2
while len(S) > 1:
    parts = partition(S, g)
    group = parts[index] if stage == part else S minus parts[index]
    test or infer the group using the evidence rules
    if group fails:
        S = group
        g = 2 if stage == part else max(g - 1, 2)
        stage = part; index = 0
        remember that a group failed
        continue
    if another piece remains in this stage:
        index += 1
        continue
    if stage == part and g > 2:
        stage = complement; index = 0
        continue
    # At g = 2, complements duplicate the two parts; do not test them again.
    if g == 2 and S == original_candidates and failed duration > short duration
       and no full group was checked and no duration escalation occurred:
        test the full S at the current duration
        if it passes:
            for a located full group already at the failed duration, finish
            otherwise restart with original_candidates, g = 2, stage = part,
              index = 0, duration = failed duration, and clear failed/full flags
            mark the single allowed duration escalation
            continue
        remember the full group as checked and failed if it failed
    if g < len(S):
        g = min(2 * g, len(S)); stage = part; index = 0
        continue
    finish narrowing
# A singleton is a culprit. Otherwise use the last failing set, or fallback.
# A located hunt with no failed group first checks the full failing profile
# at the failed duration before its loaded/fallback endpoint.
```

“Recent constraint cores” means the core whose latest update made its failure point shallower, or the members of the latest recorded combination. A passed or skipped group advances the partition plan; skipping is not a passed trial and supplies no pass evidence.

A hunt of a failed trial starts directly at that trial's longer duration when `n` valid `short_trial_s` passes of the same regime, workload and loaded cores cover the failing profile, recorded after the latest reset and before the failure, and no earlier failure in that regime at `short_trial_s` or less has occurred since reset. The short-failure veto includes other workloads and profiles. Idle failures without a trial do not use this prior. The first `hunt.group` explains the duration choice and cites the supporting pass sequences; the short passes do not establish an outcome at the longer duration.

With more than two candidates, a hunt can probe the core with the most recent failure point alone before its normal binary parts. The two newest eligible attributed parked failures of the chosen trial class before the hunt's source failure must both name that core, occur after the latest reset, and have adjacent offsets: the newer failure is one count shallower than the older, and the current failing offset is one count shallower again. The core must be a candidate. The first group cites both failures and explains the ordering. A failure is handled normally; a pass returns to binary parts at index zero and never establishes an untested group.

A group passes after `n` trials; its first failure rejects it. Ledger evidence can infer an outcome: a part or complement counts the session's evidence of its class recorded after the newest `command.reset` of any core, including earlier hunts'. The same boundary applies while the group runs and to its projected pass count, so earlier valid passes combine with new trials. Passes must be at an equal or deeper profile, and failures at an equal or shallower one, under the ledger's usual rules. Locate groups, full groups and member probes infer only from evidence recorded since their hunt started and count new trials since their group started. A group reaching a new failure point or combination is skipped. `hunt.group` records the partition stage, subset, group profile, duration and outcome so interrupted hunts resume without duplicating commitments.

Carried passes and failures are admitted across these reset, hunt-start and group-start inference boundaries for every group stage, subject to reset invalidation and the usual class and profile rules. This applies both when planning an inferred group and when evaluating or projecting its running outcome.

A `hunt.group` plan and `hunt.end` cite what established each earlier group's outcome, so the cause graph of a hunt's commitments reaches every failure that shaped its subset and member offsets, not only the failure the hunt started from. For a group a trial rejected, they cite the newest admitted failure, live or carried, at an equal or shallower profile recorded before the next group was planned, unless `n` passes recorded before then cover it, as they would for the group's live outcome; a later group's failure is not cited for it. For a group whose failure was inferred, they cite that group's own `hunt.group`, which cites the known failure, live or carried. For a passing group, they cite only its carried passes; live passes are not cited. A narrowing group followed by another narrowing group keeps the outcome it had when the next group was planned, since no later decision reads it again: a failure recorded afterwards, or one that passes recorded before the next group cover, is not cited for it, and its cited carried passes are those recorded before the next group. The latest group, a located hunt's locate group, the narrowing group before the first member probe and every probe are read again, so a failure recorded at their profile after the next group can decide them and is cited.

The pre-scheduling known-failure rule also prevents a parked trial from re-running a valid failure outside its local inference window: an inferred-failure `hunt.group` cites the known failure and advances the existing group plan. This does not widen pass inference windows. A hunt entered from a skipped together trial records the known failure's sequence, original trial identity, full failing profile and class in `hunt.start`, including when the source is `trial.carried`; its message explains the skipped trial and source session.

Evidence windows are summarized here. “After” means a journal sequence strictly after the named boundary. Every row retains the class, componentwise profile, failure-invalidation and backend-identity rules in [Evidence](#evidence).

| Requirement or stage | Live passes admitted | Live failures answering the local outcome | Carried passes / failures |
|---|---|---|---|
| Ordinary search step | Its own step's R1 then R2 trials; no historical pass inference | Its own failure, or a valid known failure through the pre-scheduling rule | Passes do not replace the live step; eligible failures can skip it |
| Candidate solo-limit check | After `check_solo_limit` (or the initial checking `core.phase`) | A live failure or the pre-scheduling known-failure rule | Both admitted across the phase boundary |
| Hunt `part` / `complement`, including the optional repeated-core singleton | After the latest `command.reset` of any core, for planning, running outcome and projected count | After that same reset for local inference | Both admitted across reset/start boundaries if not discarded by reset |
| Hunt `locate` / `full` / member `probe`, while planning | After `hunt.start` | After `hunt.start` | Both admitted across the hunt-start boundary |
| Hunt `locate` / `full` / member `probe`, once running | After that `hunt.group`; earlier live partial counts do not combine with new launches | After that `hunt.group` | Both admitted across the group-start boundary |
| Backoff rerun | After the obligation's failure boundary, at the current checking profile | Normal failure handling; outside multi-core R7 a valid known failure can skip a launch | Both admitted across the obligation boundary |
| Phase-2 round check | After `deepening.round` start, at its check profile | Normal failure handling; outside multi-core R7 a valid known failure can skip a launch | Both admitted across the round boundary |
| Open cycle requirements and full-cycle coverage | After `checking.cycle` start; live hunt, rerun or other passes can count if they cover the class and profile | Normal failure handling; global invalidation still includes pre-start failures | Passes excluded; eligible failures still invalidate |
| Separate all-zero rerun before a dead end | A live rerun of the failed class at the all-zero profile | An actual failed rerun, not a known-failure skip | Neither substitutes for the rerun; an all-zero `locate` instead follows the hunt-stage rows |

A reset discards live and carried facts whose loaded set includes the reset core, and multi-core R7 facts naming that core even when it was unloaded. Request measurements are removed only when their loaded set includes the reset core; naming an unloaded reset core removes the fact but retains its request measurement. Other carried facts survive; their carried status admits them across local sequence boundaries. The latest reset of any core nevertheless moves the live `part`/`complement` boundary. All pass counts still start after the newest covering failure, including an eligible failure outside the local pass window. Outside multi-core R7, the pre-scheduling known-failure rule can reject a launch using a failure outside a group's local inference window; it never widens that group's pass window. Multi-core R7 has no such skip.

A singleton ends `culprit` with a failure point at that core's failing offset. A larger subset that failed a group is a combination; if no tested group failed it ends `fallback` with a combination over all remaining candidates at their failing offsets. Before a combination ends, unless the profile already breaks it, member probes find how shallow each member must be, one member at a time in core-id order. A probe is a group of stage `probe` with the probed member at the probe offset, members already probed held at their shallowest failing offsets, the rest of the combination at their failing offsets and every other core at its parked offset, run at the duration of the hunt's last group. For the probed member, its failing offset is the deepest known failure and its parked offset the shallowest known pass. Probes go 1, 2, 4, … counts shallower than the failing offset until one passes or would reach the known pass, then bisect between the shallowest failure and the deepest pass until they are one count apart; a skipped probe stops probing. The hunt then ends `combination`, recording each member's shallowest failing offset as the `hunt.end` `members`; the combination sits at those offsets and the combination backoff rule picks the member to move. An attributed failure during a group ends `direct` and records a failure point at its actual applied offset, even on a core held at parked offsets. `hunt.end` precedes its `backoff` or `combination`; a combination already broken by the profile needs no further backoff. Resume uses cause linkage to emit each missing commitment once. A reset cancels an open hunt and requeues its failure.

## Phase 1

Phase 1 is checking from the margin offsets ([Search](#search)). It begins when no core remains in search and ends at the first passed full cycle that ends after every core left search. That cycle's profile is the **first confirmed profile**, whether or not its cores are at their limits.

Phase 1 is one-way: it never deepens. Group failures only step cores back, by the backoff, hunt and escalation rules of [Checking](#checking), [R7 voltage-targeted backoff](#r7-voltage-targeted-backoff) and [Hunt](#hunt), all unchanged. The phase-1 cycle's end reason reads `phase 1 ends: first confirmed profile; ...`, naming the cores phase 2 can move or that none can; it is informative and no decision reads it.

Each core's solo limit is the `pass` of the `core.phase` that took it out of search. Later `pass` values do not replace it: a failure at the solo limit plus one routinely discards `pass`. A reset clears it ([Reset](#reset)).

## Phase 2

Phase 2 follows phase 1 by default. It is bounded: rounds that take margins back, then a full confirmation cycle. All its state is rebuilt from journal events, so a resume equals a fresh fold.

**Candidates.** A candidate is a core out of search with a recorded solo limit and an offset shallower than it, that is, a has-room core shallower than its solo limit. A core at solo limit 0 never is one.

**Rounds.** A round is due once phase 1 has ended, no confirmation cycle has started, and search, hunts, reruns and the open cycle are finished. Each round moves every candidate one count toward its solo limit, one `deepen` decision per core with reason `round R: one count toward solo limit L`, visiting cores in the session's CCD-alternating order. It accepts a move only if the accumulated profile reaches no failure point or combination; a recorded failure point or combination is never retried ([invariant 4](#invariants)). A core whose next count is blocked is finished. There is no misblame or charge filter: a core with an earlier group failure is blocked only by recorded constraints (#493, Q33 supersedes Q26). After the last move one `profile.change` applies them all.

`deepening.round` start records `base` and `base_seq` as phase 1's passed cycle, `target` as the solo limit for round cores and the current offset for others, `profile` as the moved profile, and `cores` as the moved cores plus any mover carried over from a round that ended unpassed without a blame (below). A passed round end makes the moved offsets those cores' last passed offsets. A core's **round baseline** is the shallower of its last passed offset and its offset when the round began (offsets are negative, so the larger number); it is not the session [baseline](#session-start). Only a carried offset that is deeper than the last passed one keeps the last passed offset as its round baseline, while a core that an ordinary backoff moved shallower than its last passed offset is judged against where it stood, so its next count is a deepened offset that its own alone checks cover and that deepened-core blame can charge. A round ends unpassed without a blame when a failure with no deepened core loaded closes it or when its profile reaches a failure point or combination before every move is made. Its moved cores keep their unverified offsets; the next round lists them in `cores` without moving them again and re-checks them in place with the workload index of the round that moved them, counting their passes since that round began. Every core tries each offset at most once. Rounds repeat while any candidate can move. A candidate moves in every round until its next count is blocked or it is finished, so the number of rounds is at most the largest candidate gap plus the number of rounds that end unpassed; a round that ends unpassed after a blame finishes every deepened mover of the round.

**Checks.** The round's checks are those of any deepened core: alone, `n` R1 trials and `n` R2 trials at `short_trial_s` for each deepened core, the round's index `(round - 1) mod 3` freezing the catalog workloads. For every R7 workload, the round also checks the partial where a deepened core belongs to the top group under the current request order, and its CCD's full part when that core is or becomes a top requester there, and, when a deepened core was the initial top, the CCD's whole load; each selected part needs `n` trials at `short_trial_s`. Checks shared by several deepened cores run once. Request order uses the proposed profile, so a rank change selects the new part and the next checking chain derivation re-orders. Carried passes and failures count across the round boundary under the usual class, profile and invalidation rules; a round accepted using carried evidence cites its carried sequences and source sessions. Resume completes missing moves and checks without duplicating decisions.

**Blame on a round failure.** A failure ends its round when a deepened core of the round was loaded: the failing trial's core for a trial alone, an R6 or R7 trial's loaded cores, every core for an idle failure. An idle failure has no trial; it belongs to the round when it was recorded after the round began and before it ended, and it is charged to the round's cores that its recorded profile (or, if it recorded none, the profile in effect) had already moved past their round baselines. One recorded before any move was applied follows the ordinary rules. The round ends with reason `failure #N: blame goes to the deepened cores`, and the failure is answered as follows. A failure with no deepened core loaded follows the ordinary rules.

1. One core is blamed, in this order: the attributed failure's core if deepened; for a multi-core R7 failure, the named culprit if deepened, else the top request group among the deepened loaded cores by the failed trial's measured requests, ties broken by preferred-core ranking, the less preferred core first, as R7 backoff does; otherwise the deepened core with the largest move, with the same tie-break.
2. Every other deepened mover of the round, loaded or not, yields (`tuner.decision`, phase `deepening`, decision `yield`) to its round baseline and is finished: no later round moves it, so no round retries an offset it tried (#493, Q21; the measurements are in [ADR 0054](../adr/0054-two-phases-replace-the-deepening-loop.md)). Ordinary backoffs can still step it back.
3. The blamed core backs off (phase `deepening`, decision `backoff`) to the shallowest of its current offset, its round baseline and one count shallower than its failure point, where the failure point is the shallower of the failed offset and its existing failure point. The failure point blocks its next count, so it is finished too. The backoff's cause is the failure, which it consumes: no other decision answers it. The ordinary rerun obligation follows as for any non-search backoff ([Checking](#checking)).

These `deepening` backoffs do not count toward [escalation](#escalation-to-a-located-hunt).

**Confirmation cycle.** Once phase 1 has ended and no round is due, the next `checking.cycle` start is phase 2's confirmation cycle, and its reason names it. This holds also when no candidate could move at phase 1's end: phase 2 is then that cycle alone. Phase 2 never concludes at phase 1's cycle. The confirmation is an ordinary cycle identified by its order.

A failure inside the confirmation, between its start and its passed end, whose failing profile has a loaded core deeper than its first-confirmed offset, returns one such core per failure to its first-confirmed offset with a failure point: the blame order above applies, top requester first, and the decision is a `deepening` backoff that consumes the failure (#493, Q29). Today's rerun-and-continue rules then apply. A failure with no deepened core loaded follows the ordinary rules, so a phase-1 core steps back only when the failure recurs with every deepened loaded core reverted. If the confirmation ends without passing, the next cycle is still the confirmation; rounds never reopen.

**Conclusion.** The confirmation's first passed full end concludes phase 2; its profile is the **confirmed profile**. Checking then continues indefinitely and never deepens again (#493, Q22): failures only step cores back, and a core steps back on its second failure within a window ([Later failures](#later-failures)). Clean cycles count from this conclusion ([Checking](#checking)).

**Reset.** A reset returns the session to phase 1 ([Reset](#reset)).

## Later failures

After phase 2 concludes, a core steps back only after two failures count against it within K cycle starts, so single background failures do not wear the profile down (#493, Q30; [ADR 0056](../adr/0056-later-failures-step-back-in-pairs.md)). K is 5. Nothing deepens, so offsets only get shallower.

**Gate.** The rule is active only when phase 2 has concluded and the current profile has passed a full cycle since it last changed: the latest passed full cycle ended after the conclusion and after the latest `profile.change`, and every core's offset equals the checking profile. While the gate is closed every failure follows the ordinary rules of [Checking](#checking), [R7 voltage-targeted backoff](#r7-voltage-targeted-backoff) and [Hunt](#hunt): in the first cycle after the conclusion, and after any step-back until the stepped-back profile passes a full cycle. A [reset](#reset) closes it until phase 2 concludes again.

**Failures that count.** A live failure that happens after the latest passed full cycle counts against its **strike set**, the cores the ordinary rule would step back for it:

- an attributed failure: its core;
- a multi-core R7 failure: the core each voltage-targeted decision would move, one per affected CCD;
- an unattributed failure: the loaded cores of its failed trial, every core for an idle failure.

Any load, class or regime pairs with any other. A known-failure skip, a carried failure, a failure at CO 0, a dead end, a hunt-phase outcome and a located hunt's outcome, including the group failure it names, are never held. A crash is a failure like any other.

**Window.** A failure's cycle is the number of the latest `checking.cycle` start at or before it. A failure pairs with an earlier held failure when their strike sets share a core, their cycles differ by less than K, and every core of the held failure's strike set is still at the offset the hold found it at. Passed cycles in between do not reset a hold; moving a held core for any other reason or the window passing drops it. A pair applies whether or not the gate is still open.

**Hold.** A failure with no earlier failure to pair with is held. The rest of the pipeline sees it as handled and nothing moves:

- For an attributed or multi-core R7 failure, the decision is a `tuner.decision` with phase `checking`, decision `backoff`, equal `from_offset` and `to_offset`, and the core's `pass` and `failure_point` unchanged. Its cause is the failure first, then the causes the ordinary step-back would have had, and its reason starts `holds` and names the failure, its cycle, the cycle by which a second failure steps the core back and K.
- For an unattributed failure, the decision is a `hunt.skipped` citing the failure whose reason starts `held` and names the loaded cores, the same deadline and K.
- A hold records no failure point, queues no rerun and does not count toward [escalation](#escalation-to-a-located-hunt); the failure stays valid evidence. It moves no core, but the [BIOS profile](#bios-profile) shows the step-back the tuner has not yet made.

A failure invalidates every earlier pass of its class at a profile at least as deep, so the failed class must pass again at the same profile before the open cycle can pass. A held failure is not a known failure for the pre-scheduling rule ([Checking](#checking)): the requirement runs, and a second failure there pairs with the first.

**Pair.** The second failure is answered by the ordinary rule: the step-back, or for an unattributed failure the hunt. The decision cites the second failure first and the held failure last in `cause`, and its reason ends `second failure within 5 cycles: #A (cycle a, held) and #B (cycle b)`, with K written as its number. Only the second failure's class gets reruns, and each hold pairs once. Every `tuner.decision` and `combination` that ends a paired hunt says `after failures #A and #B`, including the backoff that follows a hunt ending `direct`.

### BIOS profile

The BIOS profile is what the operator enters in BIOS ([ADR 0004](../adr/0004-find-only.md)). It is derived from the journal, with no event of its own:

- The last confirmed profile is the checking profile at the latest passed full cycle ending at or after phase 1's end.
- Each core shows the shallowest of its confirmed offset, its current offset and the targets of the valid holds that name it. A core deepened by a phase-2 round shows its confirmed offset until a passed cycle confirms it, and a step-back replaces the shown offset at the decision that records it, before its `profile.change`.
- A hold's target is the offset the ordinary step-back would have moved the core to: one count shallower for an attributed failure, the voltage-targeted target, possibly several counts shallower, for a multi-core R7 failure. The held `hunt.skipped` of an unattributed failure shows one count shallower for every core the skipped hunt would have searched, its candidates (the failure's nonzero members, as [Hunt](#hunt) computes them); cores at 0 stay. The hunt stays skipped and the core stays where it is, so the tuner itself still does not step back on a single failure.
- A hold's target stays shown while the hold is valid, through passed cycles. A second failure within K pairs: its ordinary step-back replaces the shown offset at the decision that records it. A paired hunt moves nothing until its commitment, so the hold's target stays shown, unconfirmed since the hold, while the hunt runs, and the hunt's commitment (the backoff or combination that ends it) or its end without one replaces it with the actual offsets. Moving a held core for any other reason also drops the hold, and the shown offset follows the core. When K cycle starts pass with no second failure the hold expires and the core shows its held offset again, which has passed those cycles.
- A core is unconfirmed when it shows an offset other than its confirmed one, which includes a core shown stepped back by a hold. The profile is unconfirmed while any core is, since the earliest event that marked a core, until the next passed full cycle ends: that cycle confirms what the profile then is, except for the cores a valid hold still shows stepped back. An expired hold's core shows its held offset, which equals the confirmed profile, so it is confirmed again.
- Before phase 1 ends there is no confirmed profile.

The shown profile never carries an unresolved failure (Q30): a held failure is shown stepped back at once, and a step-back from a second failure, or from a failure while the gate is closed, replaces it at once.

## Defect list

Known defects identify decisions made under earlier builds whose decisions cannot safely be rerun. On resume, their journal causes and build fixes stamps are matched as specified in `journal.md`. A too-cautious finding leaves checking running and `status` names the affected cores' individual reset commands. A too-aggressive finding stops unattended tuning until an operator has answered a terminal reset prompt. Resetting a core clears its failure point and the combinations naming it, then searches from its baseline.

## Reset

- `reset --core N`: records `command.reset` and queues the reset. The next `run`, after pending attribution, cancels an open hunt or phase-2 round and ends an open cycle without passing, then records `core.phase` to `search` at the baseline clamped to [-50, 0]. It clears the core's failure point and pass and every combination that includes it (listed in `cleared_combination`). A failure point at 0 is cleared too. It also returns the session to [phase 1](#phase-1): the phase state clears, including that core's solo limit and any conclusion, and is rebuilt at the next phase-1 passed full cycle, after which a new phase 2 and confirmation follow.
- `reset --all`: archives the session (`journal.md`). The next `run` starts a new session with a fresh baseline and BIOS context, carrying nothing from the archived one.

`reset` writes to the journal, so it refuses while a `run` holds it (`journal.md`).

## Dead ends

togi stops when it cannot make progress:

| Condition | Why it cannot continue |
|---|---|
| Attributed failure at offset 0 outside multi-core R7, or an unattributed together failure with every core at 0. In multi-core R7: a named core at 0 that was a top requester of its CCD in that trial, or an unattributed failure with every loaded core of each affected CCD at 0, once its located hunt ended `loaded` when an unloaded core was off CO 0. Unless the failing profile was all at 0, only once the failing trial's all-zero rerun failed too | The instability is not caused by Curve Optimizer. |
| SMU readback differs from the written value, or an SMU command fails | Offsets can no longer be trusted. |
| The same backend has `dead_ends.inconclusive_in_a_row` consecutive inconclusive trials (3 by default), then three more consecutive inconclusive trials after waits of 60, 300 and 1800 seconds before those trials, or is missing | No evidence can be produced; the `no_evidence` dead end fires at `dead_ends.inconclusive_in_a_row + 3` (6 by default). |
| 3 stray crashes in a row | The machine crashes before togi acts: a boot loop. |
| A backend thread observed outside its allowed logical CPUs | Attribution is broken. |
| Preflight fails | The environment is not the one being tuned. |
| An unanswered too-aggressive defect without a terminal | Earlier decisions may have moved offsets deeper than proven; an operator must decide whether to reset the affected cores. |
| Thermal-trip reset without higher-precedence failure evidence | Cooling must be checked before tuning again. |

A `failure_at_zero` dead end is never taken on the failing trial alone unless its profile was already all at CO 0. Before it, the failing trial reruns with every core at CO 0: a `trial.intent` with condition `parked`, `rerun`, no `hunt`, an all-zero `profile` and the failed trial's regime, workload, loaded cores and duration, with `core` and no `cores` when the failed trial was single-core, in phase `hunt` when the failure was parked and `checking` otherwise, citing the failure. A known failure never answers it, and an inconclusive rerun is retried. If it fails, the dead end stands on an observed claim and cites both failures. If it passes, Curve Optimizer is involved: nothing dead-ends, and the failure goes to the cores off CO 0 ([Checking](#checking), [R7 request order and attribution](#r7-request-order-and-attribution)). A located hunt whose loaded cores were all at CO 0 needs no separate rerun: its locate is one ([Hunt](#hunt)). A transition carries a failure point at CO 0 only for a confirmed failure, whose failing profile was all at CO 0 or whose all-zero rerun failed; an unconfirmed failure at CO 0 carries only as a fact, and the new session routes it like any carried failure ([journal.md, Transitions](journal.md#transitions)).

What a dead end does in each run mode is in `runtime.md`. Thresholds are configurable.

A dead end follows from evidence recorded in the journal, not from memory, so a kill between the evidence and the `deadend` event still stops the next `run` before any SMU write. The evidence:
- an `smu.error`, or an `smu.readback` whose offset differs from `expected`;
- a `trial.end` with `escaped` CPUs;
- a backend's streak of inconclusive `trial.end`s reaching `dead_ends.inconclusive_in_a_row + 3`, after waits of 60, 300 and 1800 seconds before the three trials following the initial `dead_ends.inconclusive_in_a_row` consecutive inconclusive trials; trials interrupted by a stop or restart do not count;
- the stray-crash streak reaching the threshold.

A `deadend` consumes the evidence it reports: the SMU flag, escape flag, thermal trip, inconclusive streak or stray streak. Other conditions are evaluated fresh by the following `run`. Preflight repeats every run. A failure at 0 leaves failure point 0 on its core, so every later run stops until reset.

That fresh evaluation applies only after the dead end has recorded its boot action and `shutdown`. If a process stops between `deadend` and those events, the next `run` finishes that same dead-end action and exits without making a tuning decision.

