# togi

togi finds the deepest per-CCD Curve Optimizer offsets a Zen 5 desktop CPU sustains, tests them on whole profiles at night and observes them by day: it reports evidence, not a durability grade.

The glossary governs prose and display. Wire names are frozen: a vocabulary change renames messages, help and docs, never an event kind or field ([journal.md](docs/spec/journal.md#event-format)) or a configuration key ([runtime.md](docs/spec/runtime.md#configuration)), so it never bumps `togi_journal::SCHEMA` on its own.

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
The complete ordered per-core offsets and their tuning-unit mapping; in the coarse phase, every member core receives its CCD's offset.
_Avoid_: config, preset, vector, resident profile

**Baseline**:
The verified all-zero profile at session start ([Session start](docs/spec/tuner.md#session-start)).
_Avoid_: stock, default

**BIOS context**:
The identity of the firmware and CPU a session is tuned under: BIOS version, board, CPU model, microcode and boost limit.

### Testing

**Session**:
One tuning effort, from the first `run` until `reset --all` or a transition, spanning any number of reboots and BIOS changes.

**Transition**:
The end of a session written by an older ruleset, schema or evidence epoch, with no newer dimension: the successor starts at verified stock and imports compatible original-identity observations and campaign history, never permission ([Transitions](docs/spec/journal.md#transitions)). A BIOS-context change stays within the session.

**Backend**:
An external stress program togi drives: mprime or y-cruncher.
_Avoid_: tool, stressor

**Backend identity**:
The package store path `config.loaded` records for a backend, such as `/nix/store/…-y-cruncher-0.8.7.9547`. What it changes about evidence is in [Evidence](docs/spec/tuner.md#evidence).
_Avoid_: backend version

**Regime**:
One of nine classes of load, R1 to R9, each exercising a different operating condition.
_Avoid_: test type, mode, stage

**Workload**:
A concrete backend configuration within a regime, such as mprime SSE with FFT sizes 4K-21K.
_Avoid_: test

**Trial**:
One launch of one workload on its target under a fixed profile, ending as a pass, a failure or inconclusive. The unit of pass evidence because failures can cluster at onset.
_Avoid_: start, test, iteration, run

**Partial part**:
An R7 load of fewer cores than a CCD's full load, derived from measured request order, to test the remaining cores.

**Trial class**:
The regime, workload, sorted loaded cores and duration of a trial. Evidence transfers by class ([Evidence](docs/spec/tuner.md#evidence)).

**Target**:
The core or cores a trial loads.

**Night**:
The testing period the operator ends: identified by its opening `night.plan` sequence, it runs until the operator stops it or the next normal boot begins, a dead end stops togi, or every cell it could run needs an unavailable backend. It has no end time ([Night declaration](docs/spec/runtime.md#night-declaration)).

**Day**:
Normal-boot use under the day offset, observed without workloads ([Day steps](docs/spec/tuner.md#day-steps)).

**Night queue**:
One of the kinds of work a night runs: mapping, control, qualification, requalification and reproduction, with the filler when none is due ([Scheduling](docs/spec/tuner.md#scheduling)).

**Mapping**:
A night queue that explores each tuning unit alone, brackets its edge U and sweeps toward its hard ceiling C ([Night edge and hard ceiling](docs/spec/tuner-ruleset14.md#2-night-edge-and-hard-ceiling)).

**Control**:
A night queue that runs the whole coverage matrix at all-zero, on the first night and after a firmware change ([CO-0 controls](docs/spec/tuner.md#co-0-controls)).
_Avoid_: baseline run

**Qualification**:
A night queue that builds a composite certificate for one exact day profile, reusing cells of an unchanged CCD where the coverage matrix allows; only a complete, current one lets that profile become the day offset ([Coverage matrix](docs/spec/workloads.md#coverage-matrix)).

**Requalification**:
The qualification of the current day offset when its certificate is due for renewal or invalidated ([Night order and reserved budgets](docs/spec/tuner.md#night-order-and-reserved-budgets)).

**Reproduction**:
A night queue that re-runs a CPU-suspect daily event's failed profile, then each CCD alone, for one night. It never qualifies; it ends reproduced, unreproduced or untestable.

**Filler**:
The work a night does when no queue has due work: repeats of exact-current-D qualification cells ([Night scheduling](docs/spec/tuner-ruleset14.md#7-reconcile-every-current-decision-path)).

**Share**:
A queue's soft percentage of the night's elapsed observed time. A queue at or over its share yields only while another with due work is under its own; it is neither a cap nor a guarantee.

**Elapsed observed time**:
What a night's tasks consumed, filler included, each counted from its `task.start`: startup, warm-up, execution, teardown and downtime after a crash until recovery closes the trial. Operator-stopped idle time is excluded. It is not exposure, which counts eligible measured execution only ([Scheduling](docs/spec/tuner.md#scheduling)).

**Unavailable backend**:
A backend excluded for the rest of a night after its trials stayed inconclusive through their retries; no cell that needs it runs, and the night's plan names it.

**Inconclusive**:
A trial outcome that says nothing about stability, such as a backend that failed to start.

### Evidence

**Failure**:
Evidence of instability: a computation error, a stall, a corrected MCE, an uncorrected MCE or a crash.

**Crash**:
The machine rebooted or froze without a clean shutdown, detected on the next boot.
_Avoid_: hang, freeze, reset

**Evidence epoch**:
The compatibility version of trial outcomes: workload content, the backend configuration togi generates, intended durations, and pass/failure detection ([Evidence compatibility](docs/spec/workloads.md#evidence-compatibility)).

**Day offset (D)**:
A tuning unit's applied offset on normal boots; 0 before its first step ([Clean hold and steps](docs/spec/tuner-ruleset14.md#4-a-clean-hold-earns-one-step)).
_Avoid_: resident offset

**Cap (S)**:
A non-authorizing statistical diagnostic projection of isolated night evidence; it neither permits promotion nor forces retreat in Ruleset 14.
_Avoid_: limit

**Edge (U)**:
A tuning unit's isolated night edge: *unknown*, *observed* at its shallowest failing offset, or *censored* at the write limit when nothing failed ([Night edge and hard ceiling](docs/spec/tuner-ruleset14.md#2-night-edge-and-hard-ceiling)).
_Avoid_: failure offset

**Ceiling (C)**:
The hard night depth limit for a tuning unit, five counts shallower than its known edge, capped at zero.

**Tuning unit**:
A CCD in the coarse phase or a physical core in later refinement; the identity to which a day offset, mapped edge and finish reason belong.

**Machine allowance**:
The campaign's lifetime ledger of two unique CPU-suspect learning events, shared across phases and retained through reclassification and transitions.

**Slope**:
How fast a class's failure rate grows with depth: a *fallback* until enough failures are observed, then *measured* from them ([Measured slope](docs/spec/tuner.md#measured-slope)).

**Coverage**:
A class is *covered* for the cap when mapping adequately exposed it at the CCD; any other class is *uncovered* for the cap, which never means safe ([Adequate exposure](docs/spec/tuner.md#adequate-exposure)).

**Hold**:
Clean daily observation of the current exact applied profile, including the initial stock profile; fifty hours earn one bounded step subject to exact qualification and the other permission prerequisites.

**Day step**:
The cold-boot decision of the day policy: hold, promote, retreat, restore or firmware-zero. A proposal commits only after a verified readback.

**Veto**:
The ban a CPU-suspect daily event puts on its whole profile and every profile at least as deep on every physical core; it ends only when its event is reclassified.

**Freeze**:
A manual stop on promotions; it keeps the day offset and never blocks a retreat or a firmware reset.

**Firmware validity**:
Whether qualifications, slopes and caps still belong to the current BIOS, AGESA and microcode; a change invalidates them and sets the day offset to CO 0 ([Firmware](docs/spec/tuner.md#firmware)).

**Ruleset**:
The policy for tuning-unit mapping, ceilings, qualification, reproduction, day steps and campaign allowance.

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
