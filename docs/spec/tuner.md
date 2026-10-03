# Tuner

Normative rules for how togi moves offsets. Terms are defined in `GLOSSARY.md`. Regimes, workloads and failure detection are in `workloads.md`; how decisions are recorded is in `journal.md`.

## Ruleset

The ruleset is the hardcoded strategy: search strides, offset range, phases, evidence and mark rules, hunt and refinement, and guard coverage. Changing these bumps `tuner.Ruleset` (now 7) and archives an active older session into a seeded new session ([ADR 0019](../adr/0019-a-ruleset-change-starts-a-seeded-session.md), [ADR 0020](../adr/0020-hunt-and-refine.md), [ADR 0023](../adr/0023-hunts-that-converge-on-shared-voltage.md), [ADR 0024](../adr/0024-schedule-from-uncontradicted-evidence.md), [ADR 0027](../adr/0027-carry-trial-facts.md)). Changes to configurable defaults and fixes that record facts more accurately do not bump it.

The evidence epoch (`tuner.EvidenceEpoch`, now 1) separately versions compatibility of trial outcomes: workload content, backend binary or configuration, intended durations, and pass/failure detection (`workloads.md`). `session.start.evidence` records it. Without that field, a session with ruleset ≥ 6 has epoch 1; an older session has epoch 0. A transition drops passes from other epochs but keeps eligible failures. An epoch change is not a ruleset or journal-schema bump.

Transitions record eligible same-BIOS trial and idle-failure facts before `session.carried` (`journal.md`, Transitions). Ruleset 7 decides from these carried facts under the Evidence rules below; it does not carry resident offsets or qualified rotations.

## Invariants

1. Every offset stays within [-50, 0]. togi never writes a positive offset.
2. During an isolated trial only the target carries a nonzero offset. Every other core is written to 0 first, including at the start of each boot, when firmware has restored the BIOS values.
3. Every SMU write is preceded by a durable intent event and followed by a readback. A readback that differs from the written value is a dead end.
4. No applied or restored profile reaches a recorded mark, except after reset.
5. Offsets get deeper only in search and in refinement rounds.
6. Every decision is an event that names its cause (`journal.md`).

## Session start

1. Preflight (`runtime.md`) passes, or togi stops at a dead end.
2. The first `run` of a session reads every core's offset from the SMU as the baseline and records the BIOS context.
3. Each core's start offset is the configured override if one exists, else its baseline, clamped to [-50, 0]. A configured candidate edge starts the core in search at that offset with `check_edge` and frozen R1/R2 workloads; it still needs the full evidence rule.

   A session started by a transition (`journal.md`, Transitions) seeds cores from `session.carried`, citing that event after the baseline. Precedence: configured candidate edge, configured start offset, carried candidate edge, baseline. A carried candidate edge starts search with `check_edge` and reason `candidate edge <edge> carried from session <id>`. A carried failed mark is recorded in the first `core.phase`; a start at or deeper than it is clamped one count shallower, regardless of its source. A mark at 0 remains a dead end until reset.
4. A nonzero baseline produces a notice recommending BIOS CO 0 for tuning. togi still starts from it.
5. A later `run` whose BIOS context differs archives the old session and starts a new seeded session carrying edges but not marks (`journal.md`, Transitions).

## Scheduling

Cores are visited in CCD-alternating order: 0, 8, 1, 9, ... 7, 15. Each turn goes to the next core still in search. Its R2 trial follows a passing R1 in the same turn; an inconclusive trial retries the same class unless a pending reset supersedes it. Interleaving cores lets each cool between its own starts.

## Search

A search step runs one isolated R1 start then one isolated R2 start at the same offset, 90 s each by default. Its first failure rejects it. The eventual candidate edge additionally needs `n` passes in each of its frozen R1 and R2 trial classes, using `durations.search_trial_s`.

Carried passes can satisfy `check_edge` in its frozen R1/R2 classes at `durations.search_trial_s`, even though they precede the edge-check phase boundary. Ordinary search steps still require live starts.

Each core tracks its current offset `o`, `pass` (the deepest offset with a passed step, or none) and `fail` (its failed mark, or none).

After a passed step at `o`:
- `pass = o`.
- `o == -50`: check this candidate edge.
- `fail` is none: next offset is `max(o - 5, -50)`.
- Otherwise the next offset is `o - 1`. If that equals `fail`, check `o` as the candidate edge.

After an attributed failure at `o`:
- `fail = max(fail, o)`, keeping the shallowest failure.
- If `pass` is at or deeper than `fail`, it is discarded, because the new failure contradicts it.
- `o == 0`: dead end.
- `pass` is none: next offset is `min(o + 5, 0)`.
- Otherwise the next offset is `pass - 1`, or check `pass` as candidate edge if `pass - 1 == fail`.

Any failure during an isolated trial is attributed to the target, crashes included: no other core carries an offset that could explain it.

## Evidence

One start is one trial, with no internal relaunch. The pass rule is `n = ceil(ln(evidence.miss) / log1p(-evidence.rate))` consecutive passing starts, five at the defaults (0.05, 0.5). The first failure rejects a step. The full count applies to a candidate edge's R1 and R2 classes, every hunt mask and every refinement check.

A trial class is `(regime, workload, sorted loaded cores, duration_s)`. The ledger records each conclusive trial's class, sequence, applied `trial.intent.profile` and outcome. A pass at profile Q counts toward P only when Q is at least as deep as P, and not before the latest failure of that class at a profile at least as shallow as P. A failure at Q rules out P when Q is at least as shallow as P. Requirements on the same class within a step add, so a start counts once. An idle crash has wildcard R6 class with all cores loaded and invalidates all such R6 classes. Passes at deeper profiles can survive backoff; deeper moves need new evidence.

The ledger folds `trial.carried` and `failure.carried` before live evidence, ordered by their sequence in the new journal and marked as carried. Carried passes count across the local sequence boundaries for candidate-edge checks, hunt planning and outcomes (including projected pass counts), rerun obligations and refinement checks. Rotation qualification uses only live passes since its rotation start. Carried failures count everywhere failures count, regardless of those local boundaries: they invalidate earlier covered passes, keep a class failing until `n` newer covering passes, and participate in monotonicity warnings. A core reset discards carried evidence under the same loaded-core rules as live evidence.

An evidence-backed decision cites the accepted carried events' sequences in the new journal, not their old sequences, and its reason names the original source sessions. Copying a fact does not create another start or live exposure.

Before scheduling any trial, the tuner checks whether its class has a valid failure at a componentwise equal-or-shallower profile. A failure is valid until `n` newer passes in that class cover the scheduled profile. Both live and carried failures qualify; deeper, incomparable and different-class failures do not. The trial is not run: a `failure` decision records that it was skipped and why, cites the known `trial.end`, `trial.carried` or idle-failure sequence, and activates the same attribution, backoff or hunt path as a live failure. It adds no start, exposure or new failure fact. Ordinary search still needs live passes, but its known failures need not be re-observed.

A failure contradicting `n` valid passes on a profile at least as deep in the same class records `tuner.warning` `monotonicity`, without changing the failure decision. An idle failure checks all-core R6 classes individually before its wildcard invalidation; if several qualify, the warning cites exactly `n` passes from the class with the earliest valid pass.

## Marks

A failed mark is reached when `P[c] <= fail[c]`; a joint mark is reached when every member `m` has `P[m] <= J[m]`. A core is done at -50 or if one count deeper would reach a mark. Re-evaluate done after every mark or offset change. Marks accumulate until reset.

Among safe profiles, choose the greatest total depth (most negative sum of counts); preferred-core ranking breaks ties, then core-id order. A joint-mark backoff first looks for a tested move: a passing edge probe of the hunt that moved one member with every other member at its failing offset. Of those whose move leaves the resident profile reaching no mark, it takes the one moving its member the fewest counts, breaking ties toward the lowest-ranked member, and moves that member to the probe offset, citing the mark and the probe. Without one, it chooses the member leaving the most depth reachable, breaking ties toward the lowest-ranked member, and moves it one count past the mark. Only single-core attributed and hunt-culprit marks carry to a new session, not joint marks.

## Isolated trial sequence

Between trials every core is at 0. Before its first isolated trial, a `run` writes every core to 0 with one set-all command and reads each back, then records `profile.applied` with condition `isolated`. Pending decisions, a failure at 0 among them, are made before that write. One isolated trial is then:

1. `trial.intent`;
2. SMU set target to its offset and read it back (including at 0);
3. `trial.start`, plus `trial.signal` with the load-step schedule for R3 and R4;
4. the trial, then `trial.signal` with the SIGSTOP and SIGCONT counts for R3 and R4;
5. SMU set target to 0 and read it back (including when the target was 0);
6. `mce` events for the trial window;
7. `trial.end`;
8. the tuner's `failure` when the trial failed.

## Resident trial sequence

Resident trials run on the profile of the last `profile.change`. Before the first resident trial of a `run`, and after every profile change, each core is set to its profile offset in core-id order, each write read back, then `profile.applied` is recorded with condition `resident`. Masked hunts apply their recorded mask profile instead. One resident trial is then:

1. `trial.intent`, naming its loaded core for R1 to R5, or the loaded cores in `cores` for R6 and R7;
2. `trial.start`, plus `trial.signal` for R3 and R4;
3. the trial, then `trial.signal` counts for R3 and R4;
4. `mce` events for the trial window;
5. `trial.end`, naming the backend instance's core when a backend signal ended it;
6. the tuner's `failure` when the trial failed.

No SMU write happens between resident trials: the profile stays applied.

R6 loads every core. On two CCDs, an R7 guard step runs CCD0 partial, CCD0 full, CCD1 partial, CCD1 full and all-core parts; on one CCD it runs a partial before the all-core part. Each partial loads every core of its CCD except all cores tied at that CCD's shallowest offset in the resident profile when the step starts. `guard.step` freezes that starting profile and both masks across profile changes and resume. An empty mask is skipped with a journaled reason. Every nonempty part has three short starts at `start_s` and one long start at its CCD's full-part duration (counts add when durations coincide). Each start has its own intent, instance set and teardown. Only loaded cores run backend instances; the resident offsets on all cores stay applied.

Partial starts carry `trial.intent.record_only: true`. Their normal trial events, signals, MCEs and outcomes remain evidence for inspection and facts extraction, and decisive outcomes carry forward as marked `trial.carried` facts. They never enter the decision ledger: neither passes nor failures can answer a candidate edge, hunt mask, rerun, known-failure skip or refinement check, invalidate passes or qualifying profiles, produce monotonicity warnings, or derive a failed mark. The marker is not part of the trial class; exclusion applies even when another phase loads the same cores with the same workload and duration. A failed partial continues to the next scheduled start with no rerun, attribution decision, hunt, backoff or stability dead end, even at CO 0. Inconclusive starts retain the ordinary backend retries and no-evidence safety stop; containment and thermal safety stops still apply.

## Crashes

A crash is classified by what its boot recorded. A resident application counts as applied from its first nonzero `smu.intent`, before `profile.applied`. A backend signal recorded in `trial.progress`, a trial-window MCE selected by boot-local monotonic time or an uncorrected MCE in the next boot takes precedence over reset reason. Otherwise a thermal trip is a dead end; a power-button reset during a trial is its failure, but without an open trial is inconclusive. When reason reporting is supported and confirmed, no reason line means power loss and is inconclusive. An unknown or unsupported reset reason retains the usual classification:
- A trial in flight: a failure of that trial. Isolated failures belong to the target; resident and masked attribution follows Guard.
- An applied profile with no trial in flight: an idle crash, treated as an unattributed R6 failure with all cores loaded; an isolated all-zero profile changes nothing.
- Nothing applied: a stray crash. Reaching `dead_ends.stray_crashes_in_a_row` is the boot-loop dead end.

Restoring offsets before `shutdown` (`runtime.md`) is not an application: its `smu.intent` events cite `session.baseline` and leave the boot's last application as it was, so a crash part-way through is classified by what was applied before. After `profile.restored` the cores are back at togi-independent values, and a crash counts as if nothing was applied.

`crash.detected` carries the condition of the boot's last application.

A crash interrupting a record-only partial still records `crash.detected` and `trial.end`. A decisive recovered failure completes that start and continues the guard without acting on it.

A crash with a trial in flight and an idle crash are failures like any other: no rule counts them against a search or guard, caps them, or pauses after a run of them ([ADR 0018](../adr/0018-crashes-are-not-a-cost.md)). Only stray crashes are counted, for the boot-loop dead end.

## Decision events

Moves are `tuner.decision`: `step_deeper`, `check_edge`, `deepen`, `yield` and `backoff`, with phases `search`, `guard`, `hunt` or `refine`. `check_edge` freezes the R1/R2 workloads; `core.phase` to `resident` or `done` records the checked edge, pass and failed mark. Each decision cites its cause and reason. A search backoff says `failed`; other backoffs say `backed off`. A phase change after an offset or mark update re-evaluates done.

## Guard

Guard begins when no core remains in search. Its first `profile.change` has `from: null`. A profile change does not end an open rotation; passing steps on a deeper profile remain evidence after a backoff. Only `reset --core` and a covered rotation ending for refinement (Refinement) end a rotation unclean.

A rotation captures the configured schedule in its start event. R1 and R2 occurrences select successive catalog workloads (three occurrences cover each catalog); R3, R4 and R5 run on each core; R6 runs all cores. Each R7 occurrence selects its next R2 workload and requires every full part's three short and one long passing starts, with record-only partials scheduled before their respective full parts. Only full-part requirements sharing a trial class add, and trials passed on sufficiently deep profiles since the rotation start can fulfill them. The first unmet requirement of the first unmet step runs next. The rotation ends clean when all requirements pass, and qualifies when it has at least three R1, R2 and R7 steps and one each of R3, R4, R5 and R6. A partial's failure or absence never makes the rotation unclean and supplies no qualifying coverage. A non-qualifying end records the missing coverage. The newest qualifying profile, or an older eligible one when it covers the failing profile and is nowhere shallower, anchors a hunt.

Only live passes since the rotation start fulfill rotation requirements; carried passes never qualify it. Failure invalidation is global, including carried failures before that start, so the live-pass boundary does not restore contradicted evidence.

Resident and masked attribution uses the backend instance's reported core first, then exactly one core named by core-local MCEs, then the single nonzero core in the applied profile. An attributed failure records the failed mark at the applied offset and backs off that core to at least one count shallower, including when it was held at an anchor offset; failure at 0 is a dead end. If the core was already shallower, its offset need not move. An unattributed resident or idle failure queues a hunt of the failing profile and trial class instead of backing off the loaded set. An unattributed masked failure is the mask outcome. Inconclusive starts retry the same class.

A resident guard requirement with a valid known failure goes directly to that failure's attribution or hunt rather than starting the workload. Attributed skips retain the known failing offset for the failed mark and back off beyond it; unattributed skips hunt the known full failing profile and class. The decision changes pending state exactly as a live failure would, so a skipped requirement cannot repeatedly skip without making progress. The same pre-scheduling rule applies to reruns and refinement checks; a failed refinement check closes its round before the backoff or hunt.

When an attributed backoff or hunt commitment changes an offset, guard first reruns the failed class: `n` starts at `start_s`, then one at its original duration if different. These obligations are FIFO. A rerun cites the newest queued failure of its class; this does not change the obligations or their evidence windows. `run --rotations N` stops only after N clean qualifying rotation ends valid for the current profile, with every core done and no refinement able to improve total depth; without the flag guard continues indefinitely.

Carried passes can satisfy a rerun's class requirements despite preceding its obligation boundary; failures, including carried ones, retain their normal invalidation effect. When carried passes answer the rerun, the following guard-rotation or refinement-round decision cites those facts and names their source sessions. A rerun does not supply carried passes to rotation qualification.

A rotation counts only if every core was done when it ended. Ends after the last deepening count as before. An earlier end can also count if it followed the latest `command.reset`, its ending profile was at least as deep as the current profile on every core, and no failure of any trial class since that reset occurred at a profile equal to or shallower than its ending profile on every core. An incomparable failure does not contradict it; a failure with an incomplete profile conservatively prevents this credit. This credit does not change the rotation-start evidence window.

`status` reports qualified rotations since the last deepening: the count valid for the current profile, including eligible earlier credit, and the latest qualified rotation's number. `run --rotations N` checks its stop rule before starting the next rotation. A qualified rotation establishes workload breadth, not a guarantee against rare failures or untested real use.

## Hunt

Every unattributed resident failure is hunted unless its failing profile already reaches a recorded mark; then `hunt.skipped` explains why. An unattributed failure with every core at CO 0 has no candidate to hunt: it is the `failure_at_zero` dead end, naming no core. The hunt anchors on the newest clean qualifying profile raised to the failing profile: each core takes the shallower of its qualifying and failing offsets, so passes on the qualifying profile cover the anchor. A qualifying profile that is nowhere shallower than the failing profile cannot anchor; the hunt falls back to older qualifying profiles and finally all-zero. Candidates are precisely cores deeper at the failure than at the anchor. `hunt.start` records both profiles, trial class, ranking and pass rule. An idle crash has no failed trial; its trial class is guard's R6 class: the first R6 workload on every core for `durations.guard_idle_s`.

For each mask, candidates in the selected subset take their failing offsets and every other core takes its anchor offset. Delta debugging tests parts, then complements as granularity increases, retaining a failing subset. By default it starts at `start_s`; if all initial parts and complements pass, it tests the full failing profile, and if that too passes `n` starts, repeats at the failed trial's duration. At most one duration escalation occurs.

A hunt of a failed trial starts directly at that trial's longer duration when `n` valid `start_s` passes of the same regime, workload and loaded cores cover the failing profile, recorded after the latest reset and before the failure, and no earlier failure in that regime at `start_s` or less has occurred since reset. The short-failure veto includes other workloads and profiles. Idle failures without a trial do not use this prior. The first `hunt.mask` explains the duration choice and cites the supporting pass sequences; the short passes do not establish an outcome at the longer duration.

With more than two candidates, a hunt can probe the most recently marked core alone before its normal binary parts. The two newest eligible attributed masked failures of the chosen trial class before the hunt's source failure must both name that core, occur after the latest reset, and have adjacent offsets: the newer failure is one count shallower than the older, and the current failing offset is one count shallower again. The core must be a candidate. The first mask cites both failures and explains the ordering. A failure is handled normally; a pass returns to binary parts at index zero and never establishes an untested group.

A mask passes after `n` starts; its first failure rejects it. Ledger evidence can infer an outcome: a part or complement counts the session's evidence of its class recorded after the newest `command.reset` of any core, including earlier hunts'. The same boundary applies while the mask runs and to its projected pass count, so earlier valid passes combine with new starts. Passes must be at an equal or deeper profile, and failures at an equal or shallower one, under the ledger's usual rules. Full and edge masks infer only from evidence recorded since their hunt started and count new starts since their mask started. A mask reaching a new mark is skipped. `hunt.mask` records the partition stage, subset, mask profile, duration and outcome so interrupted hunts resume without duplicating commitments.

Carried passes and failures are admitted across these reset, hunt-start and mask-start inference boundaries for every mask stage, subject to reset invalidation and the usual class and profile rules. This applies both when planning an inferred mask and when evaluating or projecting its running outcome.

The pre-scheduling known-failure rule also prevents a masked start from re-running a valid failure outside its local inference window: an inferred-failure `hunt.mask` cites the known failure and advances the existing mask plan. This does not widen pass inference windows. A hunt entered from a skipped resident trial records the known failure's sequence, original trial identity, full failing profile and class in `hunt.start`, including when the source is `trial.carried`; its message explains the skipped start and source session.

A singleton ends `culprit` with a failed mark at that core's failing offset. A larger subset that failed a mask is a joint; if no tested mask failed it ends `fallback` with a joint mark over all remaining candidates at their failing offsets. Before a joint ends, unless the resident profile already breaks it, edge probes find how shallow each member must be, one member at a time in core-id order. A probe is a mask of stage `edge` with the probed member at the probe offset, members already probed held at their shallowest failing offsets, the rest of the joint at their failing offsets and every other core at its anchor offset, run at the duration of the hunt's last mask. For the probed member, its failing offset is the deepest known failure and its anchor offset the shallowest known pass. Probes go 1, 2, 4, … counts shallower than the failing offset until one passes or would reach the known pass, then bisect between the shallowest failure and the deepest pass until they are one count apart; a skipped probe stops probing. The hunt then ends `joint`, recording each member's shallowest failing offset as the `hunt.end` `members`; the joint mark sits at those offsets and the joint-mark backoff rule picks the member to move. An attributed failure during a mask ends `direct` and marks its actual applied offset, even on a core held at the anchor. `hunt.end` precedes its `backoff` or `mark.joint`; a joint mark already broken by the resident profile needs no further backoff. Resume uses cause linkage to emit each missing commitment once. A reset cancels an open hunt and requeues its failure.

## Refinement

Refinement starts only after search, hunts, reruns and the open rotation finish, when a qualified profile exists and a core is not done or a globally deeper total is reachable. When only the open rotation stands in the way and an earlier clean qualifying rotation, run with every core done on a profile at least as deep as the current one, has no failure contradicting it since, guard ends an incomplete open rotation unclean instead of running its remaining work; a fully executed rotation closes clean with its normal qualification before refinement starts; the unclean end's reason names the covering rotation and its cause cites that rotation's end. Refinement targets the safe profile with greatest total depth over all cores, with preferred-core ranking and then core-id order breaking ties. `refine.round` snapshots target, anchor and proposed profile. Cores that must become shallower yield first; those moving deeper go halfway toward their target. Each move is a decision; after the round's last move, done is re-evaluated and one `profile.change` applies them all.

Only deepened cores need checks: each runs `n` R1 starts and `n` R2 starts at `start_s`, then each R7 part containing a deepened core runs `n` starts. The round's index freezes the workload in each catalog. A failure ends the round, triggers its attribution or hunt, and a passed round records its end; resume completes missing moves and checks without duplicating decisions. A new mark that makes the proposed profile unsafe ends the round without applying it.

Carried passes and failures count across the refinement-round boundary for these checks, under the same class, profile and invalidation rules. A round accepted using carried evidence cites its carried sequences and source sessions.

## Defect list

Known defects identify decisions made under earlier builds whose decisions cannot safely be rerun. On resume, their journal causes and build fixes stamps are matched as specified in `journal.md`. A too-cautious finding leaves guard running and `status` names the affected cores' individual reset commands. A too-aggressive finding stops unattended tuning until an operator has answered a terminal reset prompt. Resetting a core clears its failed mark and the joint marks naming it, then searches from its baseline.

## Reset

- `reset --core N`: records `command.reset` and queues the reset. The next `run`, after pending attribution, cancels an open hunt or refinement round and ends an open rotation unclean, then records `core.phase` to `search` at the baseline clamped to [-50, 0]. It clears the core's failed mark and pass and every joint mark that includes it (listed in `cleared_joint`). A failed mark at 0 is cleared too.
- `reset --all`: archives the session (`journal.md`). The next `run` starts a new session with a fresh baseline and BIOS context, carrying nothing from the archived one.

`reset` writes to the journal, so it refuses while a `run` holds it (`journal.md`).

## Dead ends

togi stops when it cannot make progress:

| Condition | Why it cannot continue |
|---|---|
| Attributed failure at offset 0, or an unattributed resident failure with every core at 0 | The instability is not caused by Curve Optimizer. |
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

A `deadend` consumes the evidence it reports: the SMU flag, escape flag, thermal trip, inconclusive streak or stray streak. Other conditions are evaluated fresh by the following `run`. Preflight repeats every run. A failure at 0 leaves failed mark 0 on its core, so every later run stops until reset.

That fresh evaluation applies only after the dead end has recorded its boot action and `shutdown`. If a process stops between `deadend` and those events, the next `run` finishes that same dead-end action and exits without making a tuning decision.

