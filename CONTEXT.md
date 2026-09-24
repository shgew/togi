# shycler

shycler finds the deepest per-core Curve Optimizer offsets a Zen 5 desktop CPU sustains, then keeps testing them to measure how durable they are.

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
One tuning effort under one BIOS context, from the first `run` until `reset --all`, spanning any number of reboots.

**Backend**:
An external stress program shycler drives: mprime or y-cruncher.
_Avoid_: tool, stressor

**Regime**:
One of seven classes of load, R1 to R7, each exercising a different operating condition.
_Avoid_: test type, mode, stage

**Workload**:
A concrete backend configuration within a regime, such as mprime SSE with FFT sizes 4K-21K.
_Avoid_: test

**Trial**:
One run of one workload on its target under a fixed profile, ending as a pass, a failure or inconclusive.
_Avoid_: test, iteration, run

**Target**:
The core or cores a trial loads.

**Isolated**:
A trial condition where only the target carries its offset and every other core sits at 0.

**Resident**:
A trial condition where the whole profile is applied.

**Search**:
The phase that finds a core's candidate edge with short isolated trials.

**Confirmation**:
The phase that checks a candidate edge with longer isolated trials across R1 to R5.

**Guard**:
The endless phase that runs resident trials across all regimes once every core is confirmed.

**Rotation**:
One complete pass through the guard schedule.

**Inconclusive**:
A trial outcome that says nothing about stability, such as a backend that failed to start.

### Evidence

**Failure**:
Evidence of instability: a computation error, a stall, a corrected MCE, an uncorrected MCE or a crash.

**Crash**:
The machine rebooted or froze without a clean shutdown, detected on the next boot.
_Avoid_: hang, freeze, reset

**Stray crash**:
A crash that happened before shycler applied its profile in that boot.

**Attributed**:
A failure whose evidence names one core.

**Unattributed**:
A failure whose evidence names no single core.

**Failed mark**:
The shallowest offset at which a core has had an attributed failure since its last reset.

**Edge**:
A confirmed core's current offset; the value shycler reports for entering into BIOS.
_Avoid_: stable value, optimal offset, result

**Backoff**:
Moving a core shallower after a failure.

**Proven backoff**:
A backoff caused by an attributed failure; it sets the failed mark.

**Suspect backoff**:
A precautionary backoff caused by an unattributed failure.

**Unproven depth**:
The counts a core gave up through suspect backoffs and has not regained.

**Escalation window**:
The span after a suspect backoff, lasting until a rotation completes clean, in which a further unattributed failure backs off every core.

**Clean hours**:
The duration of passed resident trials since the last profile change.

**Tier**:
The durability rank of the current profile: Bronze, Silver, Gold or Platinum.
_Avoid_: score, level, stable

**Certificate**:
The rendering of a profile's tier with the evidence behind it.

**Dead end**:
A condition under which shycler cannot make progress and stops itself.

### Runtime

**Tuning boot**:
A boot into the shycler boot entry, where shycler runs unattended and crash reboots return to the same entry.

**In-session run**:
`shycler run` started by hand on a normal boot.

**Journal**:
The append-only record of every action shycler took and every decision it made; the source of truth.
_Avoid_: log, history, database

**Event**:
One entry of the journal.

**State**:
The current situation derived from the journal, kept as a file for readers.
