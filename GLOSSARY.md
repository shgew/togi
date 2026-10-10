# togi

togi finds the deepest per-core Curve Optimizer offsets a Zen 5 desktop CPU sustains, then keeps testing them for breadth: it reports clean cycles, not a durability grade.

The glossary governs prose and display. Wire names are frozen: a vocabulary change renames messages, help and docs, never an event kind or field ([journal.md](docs/spec/journal.md#event-format)) or a configuration key ([runtime.md](docs/spec/runtime.md#configuration)), so it never bumps `journal.Schema` on its own.

## Language

### Hardware

**Core**:
A physical CPU core, identified by the kernel's `core_id` (0-15 on a 16-core part).
_Avoid_: CPU, thread

**Logical CPU**:
One SMT thread of a core, the kernel's `cpuN`; every core has two.
_Avoid_: core, hyperthread

**CCD**:
One of the two core dies, holding eight cores each.
_Avoid_: CCX, chiplet

**Offset**:
A core's Curve Optimizer value in counts, from -50 to 0.
_Avoid_: undervolt, voltage, CO value

**Deeper / Shallower**:
More negative / less negative offset.
_Avoid_: higher, lower, increase, decrease

**Profile**:
The offsets of all cores at one moment.
_Avoid_: config, preset, vector, resident profile

**Baseline**:
The profile the SMU reported when a session started, normally the BIOS values.
_Avoid_: stock, default

**BIOS context**:
The identity of the firmware and CPU a session is tuned under: BIOS version, board, CPU model, microcode and boost limit.

### Testing

**Session**:
One tuning effort under one BIOS context, from the first `run` until `reset --all` or a transition, spanning any number of reboots.

**Transition**:
The end of a session written by an older ruleset, schema or evidence epoch, with no newer dimension, or of a compatible session whose BIOS context changed: the next `run` archives it and starts a new session seeded from eligible evidence ([Transitions](docs/spec/journal.md#transitions)).

**Backend**:
An external stress program togi drives: mprime or y-cruncher.
_Avoid_: tool, stressor

**Backend identity**:
The package store path `config.loaded` records for a backend, such as `/nix/store/…-y-cruncher-0.8.7.9547`. What it changes about evidence is in [Evidence](docs/spec/tuner.md#evidence).
_Avoid_: backend version

**Regime**:
One of seven classes of load, R1 to R7, each exercising a different operating condition.
_Avoid_: test type, mode, stage

**Workload**:
A concrete backend configuration within a regime, such as mprime SSE with FFT sizes 4K-21K.
_Avoid_: test

**Trial**:
One launch of one workload on its target under a fixed profile, ending as a pass, a failure or inconclusive. The unit of pass evidence because failures can cluster at onset.
_Avoid_: start, test, iteration, run

**Top requester**:
Globally, the loaded core with the highest voltage request sets the shared core rail voltage. R7 attribution uses a separate top-request group on each loaded CCD, with cores within 1 mV of that CCD's highest request tied. A CCD's top group need not contain the global rail maximum ([R7 request order and attribution](docs/spec/tuner.md#r7-request-order-and-attribution)).

**Self-sufficient**:
A core with observed passing evidence as a top requester for a given R7 workload at an equal or deeper offset. An evidence descriptor, not proof of future stability or permission to tolerate failures ([R7 self-sufficiency evidence](docs/spec/tuner.md#r7-self-sufficiency-evidence)).

**Partial part**:
An R7 load that idles the top requesters found so far, in request order, to test the remaining cores.

**Multi-core / single-core R7**:
An R7 trial loading at least two cores / exactly one core. Multi-core R7 uses request-based attribution and voltage-targeted backoff; single-core R7 retains the ordinary first-failure rules. Checking's partial chain never launches a single-core part.

**Trial class**:
The regime, workload, sorted loaded cores and duration of a trial. Evidence transfers by class ([Evidence](docs/spec/tuner.md#evidence)).

**Target**:
The core or cores a trial loads.

**Alone**:
A trial condition where only the target carries its offset and every other core sits at 0.
_Avoid_: isolated

**Together**:
A trial condition where the whole profile is applied.
_Avoid_: resident

**Parked trial**:
A trial condition retaining the failed trial's workload and load, with a group's cores at failing offsets and all others at parked offsets.
_Avoid_: masked

**Parked offsets**:
The offsets at which a hunt group holds every core outside its selected candidates ([Hunt](docs/spec/tuner.md#hunt)).
_Avoid_: anchor

**Group**:
One hunt trial plan: the selected cores kept at failing offsets, with every other core at parked offsets ([Hunt](docs/spec/tuner.md#hunt)).
_Avoid_: mask

**Search**:
The phase that finds and checks a core's candidate solo limit with R1 and R2 trials alone.

**Hunt**:
Parked trials that identify the core or combination behind an unattributed failure outside multi-core R7, or a located hunt's unloaded cores.

**Located hunt**:
The hunt of an unattributed multi-core R7 failure whose unloaded cores were not all at 0, started only after its load's voltage-targeted backoffs stopped helping: two since its last passing trial, or no loaded core able to step back ([Escalation](docs/spec/tuner.md#escalation-to-a-located-hunt)). Its first group, locate, reruns the failed load with the unloaded cores at 0 ([Hunt](docs/spec/tuner.md#hunt)).

**Member probe**:
A hunt's parked trial after it finds a combination, which moves one member to find how shallow it must be for the combination to pass ([Hunt](docs/spec/tuner.md#hunt)).
_Avoid_: edge probe

**Margin**:
The one count by which a core's checking starts shallower than its solo limit. A method constant, not fitted to any machine ([Phase 1](docs/spec/tuner.md#phase-1)).
_Avoid_: tolerance, safety offset

**Phase 1**:
Checking from the margin offsets until the first passed full cycle that ends after every core left search. It never deepens: group failures only step cores back ([Phase 1](docs/spec/tuner.md#phase-1)).

**Phase 2**:
The bounded phase after phase 1: rounds that move each candidate one count toward its solo limit, then the confirmation cycle ([Phase 2](docs/spec/tuner.md#phase-2)).

**Deepening**:
Phase 2's rounds: each moves every candidate one count toward its solo limit and checks the moved cores. Nothing deepens outside search and these rounds.
_Avoid_: refinement, refine

**Confirmed profile**:
The profile of phase 1's first passed full cycle (the first confirmed profile), then that of every later passed full cycle: the confirmation cycle's, then each cycle of indefinite checking.

**Held failure**:
After phase 2 concludes, the first failure against a core: nothing moves, and a second failure against the same core within K cycles steps it back ([Later failures](docs/spec/tuner.md#later-failures)).

**BIOS profile**:
The profile to enter in BIOS: the last confirmed profile with every later step-back applied ([BIOS profile](docs/spec/tuner.md#bios-profile)).

**Unconfirmed profile**:
A BIOS profile with a core stepped back, or a held failure still valid, since the last confirmed profile; the next passed full cycle confirms it.

**Confirmation cycle**:
The cycle that starts once phase 1 has ended and no round is due. Its first passed full end concludes phase 2; checking then continues indefinitely without deepening ([Phase 2](docs/spec/tuner.md#phase-2)).

**Checking**:
Testing together across all regimes; it continues indefinitely after phase 2 concludes.
_Avoid_: guard

**Cycle**:
One pass through the configured checking schedule, whose requirements are trials.
_Avoid_: rotation

**Full cycle**:
A cycle covering every R1 and R2 workload on every core, every part of every R7 workload occurrence, partials included, and R3, R4, R5 and R6 ([Checking](docs/spec/tuner.md#checking)).
_Avoid_: qualifying rotation

**Clean cycle**:
A passed full cycle that ended at or after phase 2's conclusion. The phase-1 cycle never counts, and nothing earlier is credited. A passed cycle can be not clean because it ended before the conclusion, not because it ended on a failed trial ([Checking](docs/spec/tuner.md#checking)).
_Avoid_: qualified rotation

**Passed cycle**:
A cycle whose requirements all passed. Failures and backoffs inside it do not end it; it can pass after dozens of them. For example, cycle 1 can fail an R7 part, back off and rerun it, then fulfill its remaining requirements and end passed, still as cycle 1. It need not be full or end with every core at its limit.
_Avoid_: clean rotation

**Inconclusive**:
A trial outcome that says nothing about stability, such as a backend that failed to start.

### Evidence

**Pass rule**:
The number of passing launches needed to accept a trial class: `ceil(ln(evidence.miss) / log1p(-evidence.rate))`. `evidence.rate` is the failure probability per independent trial that the rule aims to detect; `evidence.miss` is the maximum probability of missing that rate by observing only passes. Defaults 0.5 and 0.05 require five passes. These are heuristic settings, not a bound on every real failure mode ([Evidence](docs/spec/tuner.md#evidence)).

**Failure**:
Evidence of instability: a computation error, a stall, a corrected MCE, an uncorrected MCE or a crash.

**Crash**:
The machine rebooted or froze without a clean shutdown, detected on the next boot.
_Avoid_: hang, freeze, reset

**Stray crash**:
A crash that happened before togi applied its profile in that boot.

**Attributed**:
A failure whose evidence names one core.

**Unattributed**:
A failure whose evidence names no single core.

**Failure point**:
A failure's shallowest ruled-out offset for a core, set by [attribution](docs/spec/tuner.md#checking), a [hunt](docs/spec/tuner.md#hunt) culprit, an [R7 backoff](docs/spec/tuner.md#r7-voltage-targeted-backoff) or a [carried failure point](docs/spec/journal.md#transitions) ([Failure points and combinations](docs/spec/tuner.md#failure-points-and-combinations)).
_Avoid_: failed mark, mark of a core

**Combination**:
A failed combination of offsets on multiple cores; profiles at least as deep on every member reach the combination.
_Avoid_: joint mark

**At its limit**:
A core at -50, or one for which one more count deeper would reach a failure point or combination ([Failure points and combinations](docs/spec/tuner.md#failure-points-and-combinations)).
_Avoid_: done

**Has room**:
A core that has finished search but is not at its limit: at least one count deeper is within [-50, 0] and does not reach a failure point or combination. A descriptive phase: only a has-room core shallower than its solo limit is a phase-2 candidate.
_Avoid_: resident (core state or phase)

**Carried failure point**:
A failure point a transition brings into the new session, recorded in `session.carried` with the session and `seq` it came from ([Transitions](docs/spec/journal.md#transitions)).
_Avoid_: carried mark

**Carried fact**:
A decisive trial outcome or idle failure copied into a later same-BIOS session, retaining its original provenance. It is not a new trial or live exposure ([Evidence](docs/spec/tuner.md#evidence)).

**Evidence epoch**:
The compatibility version of trial outcomes: workload content, the backend configuration togi generates, intended durations, and pass/failure detection ([Evidence compatibility](docs/spec/workloads.md#evidence-compatibility)).

**Solo limit**:
A core's checked candidate solo limit, tested alone. Checking starts one count shallower (the margin); phase 2 may return to it.
_Avoid_: edge, stable value, optimal offset, result

**Candidate solo limit**:
An offset proposed as a core's solo limit, which still needs passing trials in its frozen R1 and R2 trial classes ([Search](docs/spec/tuner.md#search)). Search, configuration or carried evidence can supply it.
_Avoid_: candidate edge

**Backoff**:
Moving a core shallower after a failure.

**Voltage-targeted backoff**:
A backoff that raises a core's request to a voltage at which the load passed ([R7 voltage-targeted backoff](docs/spec/tuner.md#r7-voltage-targeted-backoff)).

**Proven backoff**:
A backoff after attribution or a hunt identifies a failed offset or combination, recording the failure point or combination.

**Yield**:
A move back to a core's round baseline (the shallower of its last passed offset and its offset when the failed round began; [Phase 2](docs/spec/tuner.md#phase-2)), made by every deepened mover of that round that is not blamed, loaded or not. A core that yields is finished for phase 2: no later round moves it.

**Ruleset**:
The hardcoded strategy for search, hunts, the two phases, checking and backoffs.

**Dead end**:
A condition under which togi cannot make progress and stops itself.

**All-zero rerun**:
The failing trial run again with every core at CO 0 before a `failure_at_zero` dead end ([Dead ends](docs/spec/tuner.md#dead-ends)).

### Runtime

**Tuning boot**:
A boot into the togi boot entry, where togi runs unattended and crash reboots return to the same entry.

**In-session run**:
`togi run` started by hand on a normal boot.

**Journal**:
The append-only record of every action togi took and every decision it made; the source of truth.
_Avoid_: log, history, database

**Event**:
One entry of the journal.

**Build stamp**:
The recorded version, revision, ruleset, journal schema and fixes of the togi build that started or resumed a session.

**Defect**:
A since-fixed bug that changed a recorded decision, identifiable from its cause and surrounding journal events.

**State**:
The current situation derived from the journal, kept as a file for readers.
