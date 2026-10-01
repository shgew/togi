# Tuner

Normative rules for how togi moves offsets. Terms are defined in `CONTEXT.md`. Regimes, workloads and failure detection are in `workloads.md`; how decisions are recorded is in `journal.md`.

## Ruleset

The ruleset is the hardcoded strategy: search strides, offset range, phases, evidence and mark rules, hunt and refinement, guard coverage and tiers. Changing these bumps `tuner.Ruleset` (now 5) and archives an active older session into a seeded new session ([ADR 0019](../adr/0019-a-ruleset-change-starts-a-seeded-session.md), [ADR 0020](../adr/0020-hunt-and-refine.md), [ADR 0023](../adr/0023-hunts-that-converge-on-shared-voltage.md)). Changes to configurable defaults and fixes that record facts more accurately do not bump it.

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

A failure contradicting `n` valid passes on a profile at least as deep in the same class records `tuner.warning` `monotonicity`, without changing the failure decision.

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

R6 loads every core. On two CCDs, an R7 guard step has CCD0, CCD1 and all-core parts; on one CCD it has one all-core part. Every part needs three short starts at `start_s` and one long start at its allocated R7 duration (counts add when durations coincide). Each start has its own intent, instance set and teardown. Only loaded cores run backend instances; the resident offsets on all cores stay applied.

## Crashes

A crash is classified by what its boot recorded. A resident application counts as applied from its first nonzero `smu.intent`, before `profile.applied`. A backend signal recorded in `trial.progress`, a trial-window MCE selected by boot-local monotonic time or an uncorrected MCE in the next boot takes precedence over reset reason. Otherwise a thermal trip is a dead end; a power-button reset during a trial is its failure, but without an open trial is inconclusive. When reason reporting is supported and confirmed, no reason line means power loss and is inconclusive. An unknown or unsupported reset reason retains the usual classification:
- A trial in flight: a failure of that trial. Isolated failures belong to the target; resident and masked attribution follows Guard.
- An applied profile with no trial in flight: an idle crash, treated as an unattributed R6 failure with all cores loaded; an isolated all-zero profile changes nothing.
- Nothing applied: a stray crash. Reaching `dead_ends.stray_crashes_in_a_row` is the boot-loop dead end.

Restoring offsets before `shutdown` (`runtime.md`) is not an application: its `smu.intent` events cite `session.baseline` and leave the boot's last application as it was, so a crash part-way through is classified by what was applied before. After `profile.restored` the cores are back at togi-independent values, and a crash counts as if nothing was applied.

`crash.detected` carries the condition of the boot's last application.

A crash with a trial in flight and an idle crash are failures like any other: no rule counts them against a search or guard, caps them, or pauses after a run of them ([ADR 0018](../adr/0018-crashes-are-not-a-cost.md)). Only stray crashes are counted, for the boot-loop dead end.

## Decision events

Moves are `tuner.decision`: `step_deeper`, `check_edge`, `deepen`, `yield` and `backoff`, with phases `search`, `guard`, `hunt` or `refine`. `check_edge` freezes the R1/R2 workloads; `core.phase` to `resident` or `done` records the checked edge, pass and failed mark. Each decision cites its cause and reason. A search backoff says `failed`; other backoffs say `backed off`. A phase change after an offset or mark update re-evaluates done.

## Guard

Guard begins when no core remains in search. Its first `profile.change` has `from: null`. A profile change does not end an open rotation; passing steps on a deeper profile remain evidence after a backoff. Only `reset --core` ends a rotation unclean.

A rotation captures the configured schedule in its start event. R1 and R2 occurrences select successive catalog workloads (three occurrences cover each catalog); R3, R4 and R5 run on each core; R6 runs all cores. Each R7 occurrence selects its next R2 workload and requires every part's three short and one long starts. Requirements sharing a trial class add, and trials passed on sufficiently deep profiles since the rotation start can fulfill them. The first unmet requirement of the first unmet step runs next. The rotation ends clean when all requirements pass, and qualifies when it has at least three R1, R2 and R7 steps and one each of R3, R4, R5 and R6. A non-qualifying end records the missing coverage. The newest qualifying profile, or an older eligible one, anchors hunts and refinement.

Resident and masked attribution uses the backend instance's reported core first, then exactly one core named by core-local MCEs, then the single nonzero core in the applied profile. An attributed failure records the failed mark at the applied offset and backs off that core to at least one count shallower, including when it was held at an anchor offset; failure at 0 is a dead end. If the core was already shallower, its offset need not move. An unattributed resident or idle failure queues a hunt of the failing profile and trial class instead of backing off the loaded set. An unattributed masked failure is the mask outcome. Inconclusive starts retry the same class.

When an attributed backoff or hunt commitment changes an offset, guard first reruns the failed class: `n` starts at `start_s`, then one at its original duration if different. These obligations are FIFO. A rerun cites the newest queued failure of its class; this does not change the obligations or their evidence windows. `run --rotations N` stops only after N clean qualifying rotation ends since the last deepening, with every core done and no refinement able to improve total depth; without the flag guard continues indefinitely.

## Hunt

Every unattributed resident failure is hunted unless its failing profile already reaches a recorded mark; then `hunt.skipped` explains why. An unattributed failure with every core at CO 0 has no candidate to hunt: it is the `failure_at_zero` dead end, naming no core. The hunt anchors on the newest clean qualifying profile raised to the failing profile: each core takes the shallower of its qualifying and failing offsets, so passes on the qualifying profile cover the anchor. A qualifying profile that is nowhere shallower than the failing profile cannot anchor; the hunt falls back to older qualifying profiles and finally all-zero. Candidates are precisely cores deeper at the failure than at the anchor. `hunt.start` records both profiles, trial class, ranking and pass rule. An idle crash has no failed trial; its trial class is guard's R6 class: the first R6 workload on every core for `durations.guard_idle_s`.

For each mask, candidates in the selected subset take their failing offsets and every other core takes its anchor offset. Delta debugging tests parts, then complements as granularity increases, retaining a failing subset. It initially uses `start_s`; if all initial parts and complements pass, it tests the full failing profile, and if that too passes `n` starts, repeats at the failed trial's duration. A mask passes after `n` starts; its first failure rejects it. Ledger evidence can infer an outcome: a part or complement counts the session's evidence of its class, including earlier hunts', while full and edge masks count only evidence recorded since their hunt started. A mask reaching a new mark is skipped. `hunt.mask` records the partition stage, subset, mask profile, duration and outcome so interrupted hunts resume without duplicating commitments. At most one duration escalation occurs.

A singleton ends `culprit` with a failed mark at that core's failing offset. A larger subset that failed a mask is a joint; if no tested mask failed it ends `fallback` with a joint mark over all remaining candidates at their failing offsets. Before a joint ends, unless the resident profile already breaks it, edge probes find how shallow each member must be, one member at a time in core-id order. A probe is a mask of stage `edge` with the probed member at the probe offset, members already probed held at their shallowest failing offsets, the rest of the joint at their failing offsets and every other core at its anchor offset, run at the duration of the hunt's last mask. For the probed member, its failing offset is the deepest known failure and its anchor offset the shallowest known pass. Probes go 1, 2, 4, … counts shallower than the failing offset until one passes or would reach the known pass, then bisect between the shallowest failure and the deepest pass until they are one count apart; a skipped probe stops probing. The hunt then ends `joint`, recording each member's shallowest failing offset as the `hunt.end` `members`; the joint mark sits at those offsets and the joint-mark backoff rule picks the member to move. An attributed failure during a mask ends `direct` and marks its actual applied offset, even on a core held at the anchor. `hunt.end` precedes its `backoff` or `mark.joint`; a joint mark already broken by the resident profile needs no further backoff. Resume uses cause linkage to emit each missing commitment once. A reset cancels an open hunt and requeues its failure.

## Refinement

Refinement starts only after search, hunts, reruns and the open rotation finish, when a qualified profile exists and a core is not done or a globally deeper total is reachable. It targets the safe profile with greatest total depth over all cores, with preferred-core ranking and then core-id order breaking ties. `refine.round` snapshots target, anchor and proposed profile. Cores that must become shallower yield first; those moving deeper go halfway toward their target. Each move is a decision; after the round's last move, done is re-evaluated and one `profile.change` applies them all.

Only deepened cores need checks: each runs `n` R1 starts and `n` R2 starts at `start_s`, then each R7 part containing a deepened core runs `n` starts. The round's index freezes the workload in each catalog. A failure ends the round, triggers its attribution or hunt, and a passed round records its end; resume completes missing moves and checks without duplicating decisions. A new mark that makes the proposed profile unsafe ends the round without applying it.

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

## Tiers and certificate

Tiers rank the current profile by durability, not proof. A shallow backoff need not erase valid exposure: the tier clock starts at the first `profile.change` when nothing later applies, otherwise at the later of the last profile deepening and the latest failure on a profile at least as deep as the current one. Recompute it at each profile change.

| Tier | Requirement |
|---|---|
| none | A core is not done, refinement can reach more total depth, or no clean qualifying rotation has ended since the last deepening. |
| Bronze | Every core done, refinement cannot improve total depth, and a clean qualifying rotation after the last deepening. |
| Silver | Bronze, and 24 clean hours. |
| Gold | Bronze, and 100 clean hours. |
| Platinum | Gold, and 200 field hours: real use observed by the future `observe` service with the BIOS offsets equal to the profile. Unavailable until `observe` exists; the tuner never computes it. |

The tuner records every change as `tier.change`, naming its cause. A core in search or not done, reachable refinement depth, a deepening, or missing qualifying coverage prevents Bronze. Bronze's reason is `every core is done and the profile passed a clean qualifying rotation`; Silver and Gold cite `24 clean hours since the tier clock started at #N` and `100 clean hours since the tier clock started at #N`.

Each regime and workload with clean hours `T` since the tier clock shows its failure-rate bound: with zero failures, the rate is below `3 / T` per hour at 95% confidence (rule of three). The overall bound uses all clean hours. Without clean hours there is no bound. Recorded and displayed bounds round up, never understating it.

`togi status` and `togi cert` render from a replay of the journal (`runtime.md`). The certificate shows:
- the tier with its `tier.change`, and progress towards the higher tiers;
- the profile with its `profile.change`, as a per-core table of `OFFSET` values, recorded CCDs, physical slots (core number modulo 8), failed marks, joint marks, done status and the deciding event, followed by any core decided after that `profile.change`; the offsets are the resident profile values when guard exists, otherwise the current core values, and may differ from the checked isolated edges;
- clean hours and failure-rate bounds by regime and workload, with valid start counts and the highest Tctl among counted trials with its `trial.end`;
- the BIOS context and session start;
- the SHA-256 of the journal's complete lines it rendered, and the last `seq` among them.
