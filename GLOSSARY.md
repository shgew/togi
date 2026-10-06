# togi

togi finds the deepest per-core Curve Optimizer offsets a Zen 5 desktop CPU sustains, then keeps testing them to measure how durable they are.

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
_Avoid_: undervolt, margin, voltage, CO value

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
The end of a session written by an older ruleset, schema or evidence epoch, with no newer dimension, or of a compatible session whose BIOS context changed: the next `run` archives it and starts a new session seeded from eligible evidence.

**Backend**:
An external stress program togi drives: mprime or y-cruncher.
_Avoid_: tool, stressor

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
The loaded core with the highest voltage request, setting the shared core voltage. Cores within 1 mV of the highest are tied top requesters; an all-core load has top requesters on each CCD.

**Self-sufficient**:
A core that passes as a top requester for a given R7 workload.

**Partial part**:
An R7 load that idles the top requesters found so far, in request order, to test the remaining cores.

**Trial class**:
The regime, workload, sorted loaded cores and duration of a trial. Pass evidence transfers only within this class.

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
The newest passed full-cycle profile raised to the failing profile, each core at the shallower of its two offsets, when that differs from the failing profile; otherwise the next older passed full-cycle profile raised the same way, or all-zero. A located hunt parks its loaded cores at their failing offsets and its unloaded cores at 0.
_Avoid_: anchor

**Group**:
One hunt trial plan: the selected cores kept at failing offsets, with every other core at parked offsets. Member probes vary one member's offset while holding the other members at their recorded failing offsets.
_Avoid_: mask

**Search**:
The phase that finds and checks a core's candidate solo limit with R1 and R2 trials alone.

**Hunt**:
Parked trials that identify the core or combination behind an unattributed failure outside multi-core R7, or a located hunt's unloaded cores.

**Located hunt**:
The hunt of an unattributed multi-core R7 failure whose unloaded cores were not all at 0. Its first group, locate, reruns the failed load with every unloaded core at 0 and the loaded cores at their failing offsets. A failed locate ends it `loaded` and the failure is charged to the loaded cores; a passed locate narrows only the unloaded cores.

**Member probe**:
A hunt's parked trial after it finds a combination: one member moves between its failing and parked offsets, members probed before it stay at their shallowest failing offsets and the rest at their failing offsets, to find how shallow that member must be for the combination to pass. Each member is probed in turn.
_Avoid_: edge probe

**Deepening**:
Rounds that move the profile toward the greatest total depth permitted by its failure points and combinations.
_Avoid_: refinement, refine

**Checking**:
Testing together across all regimes; it continues after search and deepening finish.
_Avoid_: guard

**Cycle**:
One pass through the configured checking schedule, whose requirements are trials.
_Avoid_: rotation

**Full cycle**:
A cycle covering every R1 and R2 workload on every core, every part of every R7 workload occurrence, partials included, and R3, R4, R5 and R6.
_Avoid_: qualifying rotation

**Clean cycle**:
A passed full cycle that ended with every core at its limit and remains valid for the current profile under checking's evidence rules.
_Avoid_: qualified rotation

**Passed cycle**:
A cycle whose requirements all passed. It need not be full or end with every core at its limit.
_Avoid_: clean rotation

**Inconclusive**:
A trial outcome that says nothing about stability, such as a backend that failed to start.

### Evidence

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
A failure's shallowest ruled-out offset for a core, established by attribution, a hunt culprit or an R7 voltage-targeted backoff since its last reset, or its carried failure point if that is shallower.
_Avoid_: failed mark, mark of a core

**Combination**:
A failed combination of offsets on multiple cores; profiles at least as deep on every member reach the combination.
_Avoid_: joint mark

**At its limit**:
A core at -50, or one for which taking one more count deeper would reach a failure point or combination. Re-evaluated when offsets, failure points or combinations change.
_Avoid_: done

**Has room**:
A core that has finished search but is not at its limit: at least one count deeper is within [-50, 0] and does not reach a failure point or combination.
_Avoid_: resident (core state or phase)

**Carried failure point**:
A failure point a transition brings into the new session: the shallowest offset of an attributed failure or hunt culprit of that core in the archived sessions, recorded in `session.carried` with the session and `seq` it came from. A BIOS change leaves it behind.
_Avoid_: carried mark

**Carried fact**:
A decisive trial outcome or idle failure copied into a later same-BIOS session, retaining its original provenance and context. It is ordinary evidence subject to the current regime's rules, not a new trial or live exposure.

**Evidence epoch**:
The compatibility version of trial outcomes: workload content, backend binary and configuration, intended durations, and pass/failure detection. Passes carry only within the current epoch; eligible failures survive an epoch change.

**Solo limit**:
A core's checked candidate solo limit, tested alone; deepening together may move its offset.
_Avoid_: edge, stable value, optimal offset, result

**Candidate solo limit**:
An offset proposed as a core's solo limit, which still needs the required passing trials in its frozen R1 and R2 trial classes. Search, configuration or carried evidence can supply it.
_Avoid_: candidate edge

**Backoff**:
Moving a core shallower after a failure.

**Voltage-targeted backoff**:
A backoff that raises a core's request to a voltage at which the load passed, using the counts needed to reach that target.

**Proven backoff**:
A backoff after attribution or a hunt identifies a failed offset or combination, recording the failure point or combination.

**Yield**:
A deepening move to a shallower offset that allows other cores to move deeper and improve total depth.

**Ruleset**:
The hardcoded strategy for search, hunts, deepening, checking and backoffs.

**Dead end**:
A condition under which togi cannot make progress and stops itself.

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
