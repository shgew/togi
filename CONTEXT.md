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
_Avoid_: config, preset, vector

**Baseline**:
The profile the SMU reported when a session started, normally the BIOS values.
_Avoid_: stock, default

**BIOS context**:
The identity of the firmware and CPU a session is tuned under: BIOS version, board, CPU model, microcode and boost limit.

### Testing

**Session**:
One tuning effort under one BIOS context, from the first `run` until `reset --all` or a transition, spanning any number of reboots.

**Transition**:
The end of a session written by an older ruleset or schema: the next `run` archives it and starts a new session seeded from it.

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
One run of one workload on its target under a fixed profile, ending as a pass, a failure or inconclusive. One trial is one start.
_Avoid_: test, iteration, run

**Start**:
One launch of a trial's workload, the unit of pass evidence because failures can cluster at onset.

**Trial class**:
The regime, workload, sorted loaded cores and duration of a trial. Pass evidence transfers only within this class.

**Target**:
The core or cores a trial loads.

**Isolated**:
A trial condition where only the target carries its offset and every other core sits at 0.

**Resident**:
A trial condition where the whole profile is applied.

**Masked**:
A trial condition retaining the failed trial's workload and load, with selected cores at failing offsets and all others at an anchor.

**Anchor**:
The latest clean qualifying profile at least as shallow as the failing profile, different from it and reaching no mark; otherwise the next older eligible profile, or all-zero.

**Search**:
The phase that finds and checks a core's candidate edge with isolated R1 and R2 starts.

**Hunt**:
Masked trials that identify the core or combination behind an unattributed failure.

**Refinement**:
Rounds that move the resident profile toward the greatest total depth permitted by its marks.

**Guard**:
Resident testing across all regimes; it continues after search and refinement finish.

**Rotation**:
One pass through the configured guard schedule, whose requirements are starts.

**Qualifying rotation**:
A clean rotation covering every R1 and R2 workload on every core, every R7 workload in every part, and R3, R4, R5 and R6.

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

**Failed mark**:
The shallowest offset at which a core has had an attributed failure or been named the culprit of a hunt since its last reset, or its carried mark if that is shallower.

**Joint mark**:
A failed combination of offsets on multiple cores; profiles at least as deep on every member reach the mark.

**Done**:
A core at -50, or one for which taking one more count deeper would reach a failed or joint mark. Re-evaluated when offsets or marks change.

**Carried mark**:
A failed mark a transition brings into the new session: the shallowest offset of an attributed failure or hunt culprit of that core in the archived sessions, recorded in `session.carried` with the session and `seq` it came from. A BIOS change leaves it behind.

**Edge**:
A core's checked isolated candidate edge; resident refinement may move its offset.
_Avoid_: stable value, optimal offset, result

**Backoff**:
Moving a core shallower after a failure.

**Proven backoff**:
A backoff after attribution or a hunt identifies a failed offset or combination, recording its mark.

**Yield**:
A refinement move to a shallower offset that allows other cores to move deeper and improve total depth.

**Clean hours**:
The durations of passed resident trials after the tier clock, overall and by regime and workload.

**Tier clock**:
The later of the last profile deepening and the latest failure on a profile at least as deep as the current one. Shallower backoffs need not restart it.

**Tier**:
The durability rank of a done profile after a clean qualifying rotation: Bronze, Silver, Gold or Platinum; otherwise none.
_Avoid_: score, level, stable

**Certificate**:
The rendering of a profile's tier with the evidence behind it.

**Ruleset**:
The hardcoded strategy for search, hunts, refinement, guard, backoffs and tiers.

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
