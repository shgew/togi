# Simulating

`togi-tool-sim` (`just sim`) runs the real tuning session on a fake machine. The four-core developer script gives deterministic trial outcomes, including a computation error on an explicitly scripted nonzero load. No root or hardware is needed. The tool is not shipped in the package; run it from a source checkout.

The script exercises mechanics, not tuning policy or hardware reliability. A simulated night ends when the operator ends it (`--night-hours`) or a dead end stops it.

```sh
just sim [--seed N] [--recorded-content] [--night-hours N] [--state-dir DIR] [--samples] [--max-boots N] [--verify-every N] [--cold-boots]
```

- `--seed` (default 1) selects simulated boot identities and synthetic Tctl values. The same inputs and history reproduce the journal.
- `--recorded-content` makes simulated trials report the live journal's workload content identities, not the simulator's synthetic ones. It reads each `exposure.observed` class and `trial.intent` before any transition archives the journal, keyed by workload (R8/R9 use the underlying R1/R3 workload). This is useful on a compatible copy of recorded state ([Inspecting a transition](reviewing.md#inspecting-a-transition-on-recorded-state)). It does not verify executables, make a missing backend available or replay recorded trial outcomes. Missing identity evidence, conflicting identities for a workload and wrong-backend identities are refused before the session. A workload absent from the recording is refused when requested, not answered with a synthetic identity.
- `--night-hours` (default 8) is when the simulated operator ends the night, measured from that night's start in simulated time. Continuation of an unfinished night keeps its original start. The stop is seen at the first event appended at or after the deadline.
- `--state-dir` uses an existing directory; without it, `togi-tool-sim` creates a temporary `togi-sim-*` directory, keeps it and announces it on stderr as `sim: state directory DIR`.
- `--samples` writes `trials/<trial-id>/samples.jsonl` for later inspection, gzip-compressed on trial end; by default trial samples stay in memory. Simulation output, including compression, is never synced ([Journal, simulated storage](spec/journal.md#rules)).
- `--max-boots` (default 1000) caps the simulated boots of one invocation. A session still running after N boots stops with its journal as it stands, the reason on stderr, and exit 3.
- `--verify-every` (default 0) replays the whole journal into fresh state after every Nth simulated crash, and fails unless it equals the state the next boot resumes from; 0 checks only when the invocation ends: the session stops, an error ends it or it reaches `--max-boots`.
- `--cold-boots` replays the whole journal at every boot, as `togi run` does, instead of resuming from the state the crashed boot folded.

A crash reboots the simulated machine in-process. A warm reboot (the default) keeps the open journal and its in-memory folds. A cold reboot closes and reopens the journal and replays its events. A new invocation always reads the journal. Journal lines go to stderr as `togi run` logs them, and nothing is fsynced. When stderr is not a terminal, both the log and the appended journal lines are buffered (64 KiB each) and written at exit, including on SIGINT, SIGTERM and SIGHUP, so a killed process loses its last unwritten lines and `events.jsonl` lags the run; on a terminal the log is written line by line. The state-directory writer lock stays held across simulated reboots. Read-only commands can inspect the final state after the invocation returns.

A state directory that already holds a journal or archives resumes the simulated machine after them: boot numbering continues and the clock starts after the last event, so a crash in the new run is never mistaken for an old boot, and a session after `reset --all` gets a new id. A new invocation reads the file-backed journal before tuning; only reboots within that invocation reuse the parsed events. Real `togi run` sessions retain their file-backed recovery and per-event state writes.

The developer script has four cores. It is not a general-purpose simulator for histories of other topologies. A capped run's `machine.json` must decode with `source: "scripted"` and match the script's core count. Fitted and untagged snapshots are refused; this does not change how read-only tools decode journals.

Unrepresentable resumed timestamps, elapsed wall times and next boot numbers are refused with operation-specific errors. Only absent history uses the default epoch; arithmetic failure never resets the clock or wraps a boot identity. Clock advances, wall-clock jumps and reboots leave the previous machine state unchanged when refused.

Simulated trials keep samples in memory by default: each loaded worker's cumulative CPU milliseconds advance once per simulated second. The simulator retains only the last trial's specification, duration, stall metadata and optional fixed-size voltage/clock snapshot, generating samples lazily. With `--samples`, trials stream `trials/<trial-id>/samples.jsonl`. A script can name a stalled worker before a reset so the recovered `trial.end` exercises `stalled_core` and `worker_stalled_ms` outside R6, which suspends workers by design. For an interrupted trial, use disk samples if a later invocation needs to read the sample files.

The developer script does not emit PM-table lanes. Typed test fixtures can supply fixed voltage requests for all 16 lanes and CCD clocks for loaded cores with `ScriptedVoltageSamples`. Preflight identifies this telemetry as simulated, and the session summarizes it by the same rules as hardware. It exercises telemetry consumers, not physical voltage or clock behavior ([Conditions sampling](spec/workloads.md#conditions-sampling)).

A fresh state directory uses the default configuration, never `/etc/togi/config.json`. A state directory with a journal resumes under the configuration its latest `config.loaded` recorded: the backend store paths and dead-end settings, so passes recorded under a real machine's backend store paths keep counting. That snapshot stands in for the configuration file the real machine would load: `togi run` loads its configuration file on every start and records it in a new `config.loaded`, never reading it back from the journal, so a simulated resume matches the next real run only while that file is unchanged since the recorded `config.loaded`. The simulator names no backend packages of its own, so a fresh session records none. The session runs unattended: an unanswered too-aggressive defect is a dead end. `sim` exits 0 when the simulated operator ends the night, 1 at a dead end or on an error, 2 on a flag error and 3 at the boot cap. A journal written under an older ruleset or schema is archived and seeds a fresh session that imports only raw day records, as `togi run` does; nothing tuning-related carries ([Transitions](spec/journal.md#transitions)). A live journal of an older schema keeps only its recorded backend store paths, on top of the default configuration, because the reader for older schemas keeps nothing else of a `config.loaded`. A journal written under a newer ruleset or schema is refused before another event is appended.

The read-only commands work on the result:

```sh
just togi --state-dir <dir> status
just togi --state-dir <dir> events --core 3
just togi --state-dir <dir> watch
```

`status` reports each unit's D, hard ceiling C and tagged edge U, qualification and outstanding cells, the day policy, the night's queue shares and the Tctl peak ([Commands](spec/runtime.md#commands)).

A finished simulation's `watch` shows only its last moment. `just replay --state-dir <dir>` plays the whole journal through the dashboard on a simulated clock, 300 simulated seconds per second by default (`--speed`, which must be finite and positive), starting at `--from SEQ`; the dashboard's keys work as in `watch`. `--at SEQ` prints one frame as of that event instead, `--after 40s` that long after it, `--view help` or `--view log` for those views, with `--width`, `--height` and `--color` as for a frame on a terminal. It also plays a copied real journal.

`watch` and replay read the journal, not the sample files, so both work without `--samples`. Use that flag when inspecting the per-second sample series on disk.

Replay draws the same measured layout as `watch`: 240x67 has side-by-side CCD tables, a large trial countdown and context beside recent history; 160x45 uses an inline countdown and 120x33 compacts the lower panels. Use those sizes with `--at` to compare a recorded decision, night task or day step. Playback advances through R6 on its own clock; a live view instead holds its quiet frame until the journal records the trial's end.

Replay accepts all shipped journal schemas from the current ruleset, including schemas 1–4. Recordings from another ruleset are refused; replay needs the tuning rules used to write the journal. It translates older payload vocabulary while retaining recorded messages and configuration, so the dashboard shows the recorded night plan. An incomplete final line is ignored; replay never changes the recording.

Playback spaces consecutive events by their recorded boot-local `mono_ms` when both stamps are present and their boot IDs match; zero is a valid stamp. Across boots or with a missing stamp (including older journals), it uses the nonnegative wall-clock interval instead. The dashboard clock follows each consumed event's recorded `time`, then advances until the next event, so recorded wall-clock corrections remain visible without shortening or extending same-boot playback intervals.

Tests script explicit trial outcomes with `togi_sim::ScriptedMachine` and inject faults with the `Simulator` controls [below](#the-simulator-togi-sim). `togi_simrun::simulate` drives the real session across crashes, in memory or on disk. The developer command uses disk and defaults to warm reboots.

### The simulator (`togi-sim`)

`Simulator::scripted(ScriptedMachine, seed, History)` constructs the machine. A script supplies a checked topology, boot register values and typed outcomes. Its callback sees the trial and the full vector of applied offsets; it can pass, report a backend failure, log a corrected MCE or reset with specified evidence, including an uncorrected MCE. `History` continues boot numbering, wall time and BIOS context from a recording without treating its past outcomes as answers to new trials.

`Simulator::seams` supplies a `togi_machine::Machine` to the session. Tests can jump wall time independently of the monotonic clock, inject write/readback and trial faults, force resets, and intercept SMU accesses before and after their effects with `interrupt_smu`. A hook that resets or reboots before an access makes the access fail as `Error::Crashed` without applying it.

`togi-watchtest` supplies deterministic journal prefixes for dashboard, replay and command tests. The mapping prefixes start attested nonzero work, pass an R2 mapping trial and reach an exact R6 qualification, a machine-check reset and recovery. The stopped and dead-end prefixes exercise signal teardown and exhausted nonzero probes. The qualification, day-step, reproduction and filler prefixes use real simulated nights and staged desktop observations through the journal's public append helpers. Day writes use the session's intent, write and readback transaction.

A journal does not hold everything the next boot observes. `Simulator::persisted` returns a serializable `Persisted` snapshot of the machine. `Simulator::restore(ScriptedMachine, Persisted)` continues it with the same script, which `Simulator::source()` returns by value. The snapshot holds:

- the seed;
- the clocks;
- every boot with its reset reason and kernel log, as journald keeps them;
- the machine checks and the reset waiting for the next boot;
- the registers and BIOS context;
- the last trial's samples.

The fixed boot-ID and synthetic Tctl streams are keyed by seed and typed domain; no generator state is kept. Armed faults and recorded violations are not kept. Snapshot objects require the scripted discriminator, and a mismatched core count returns `RestoreError::Cores`.

A trial runs once: a second `wait` is an error, including after cancellation, and a trial from an earlier boot returns `Error::Crashed`. A reset retains the exposure the trial ran before it, also when the script first reports a backend failure.

### The simulation runner (`togi-simrun`)

`crates/togi-simrun` is the loop that runs whole sessions on the simulator. `simulate(SimInput)` boots the `Simulator` it is given and, on each boot:

1. Reads the boot id and BIOS context.
2. Locks the state directory and runs `togi_session::prepare`, which archives an older build transition's journal for a fresh session and prepares its raw day import. A BIOS-context change stays within the live session ([Transitions](spec/journal.md#transitions)).
3. Hands the journal and any pending raw day import to `togi_session::run`; no tuning carry is selected.
4. After a crash, reboots the machine and repeats.

The loop ends when a run stops without a crash, which gives `SimOutcome::Stopped` with the run's stop, errors, emergency outcome and resume capsule, or after `max_boots` boots, which gives `SimOutcome::Capped`. A boot whose journal starts empty begins a new session, so the simulator forgets the last trial's samples (`Simulator::forget_last_trial`), and a new trial that reuses an earlier session's trial id reads none.

The state directory is either in memory or on disk:

- **`Persistence::Memory(MemFs)`** keeps it in a `togi_journal::MemFs`.
- **`Persistence::Files(path)`** locks a directory on disk for each boot. Nothing is synced. A test that holds the directory's lock makes `simulate` fail with `SimError::Lock`.

Both modes append events to the journal. A warm reboot keeps the open journal and adopts the capsule if it covers the same journal at the same position. A cold boot or a warm fallback that has to reopen the journal replays its events. No folded state is written to disk.

For the same script, seed and inputs, both modes leave byte-identical `events.jsonl`.

**Resume and verification.** `Replay::Warm` resumes each boot from the journal and the folds the crashed boot kept (`Journal::reboot`, `togi_session::prepare_kept`). `Replay::Cold` folds the whole journal again, as `togi run` does. Verification runs in both modes. It checks the capsule against a fresh fold of the journal (`ResumeCapsule::verify`) on these boots:

- every boot that ends the simulation, by a stop or by the cap;
- every `verify_every`th crashed boot.

A divergence is `SimError::Diverged`, and a state that cannot be published is `SimError::Publish`. A crashed boot that also failed in another way, such as a failed log write, is `SimError::Failed` with those errors. A log that cannot be flushed at the end is `SimError::FlushLog`. None of these is ever reported as a cap. In a run that stops, a failed log write is `SessionError::Log` among its errors.

**Observing the run.** An `observer` sees every appended event and may answer `Observe::Stop`, which cancels the run with `Stop::Signal`. It is called once per event, in order, at constant cost, and it keeps seeing events after the caller's log fails. It may also call the simulator's fault controls, such as `crash`, `next_reset` or `power_loss`. An observer works with either persistence. `write_samples` writes trial samples under the state directory's `trials/`, so it needs persistence on disk.

**Wrapping the trial runner.** `adapt_trials`, when set, receives each boot's simulated trial runner and returns the one the session uses; it runs at every boot, so the wrapper outlives reboots, and `None` leaves the simulator's own. `togi-tool-sim --recorded-content` uses it to answer content identities from a recording; the wrapper must forward every other `Trials` call unchanged.

**After a run.** These functions read the state directory:

- `events` returns the live journal's events.
- `history` and `history_in` return a `togi_sim::History` that continues a resumed machine's boot numbering and clock.
- `resume_simulator` returns the simulator a later process continues with. A capped simulation leaves its machine as `machine.json` in the state directory, and the later process restores it. Without `machine.json`, it builds a fresh machine from `history`. A later `simulate` removes the file after its first boot that appends to the journal, so a continuation refused before it starts, by a configuration naming a missing core for example, leaves the machine for the next try. A test pins that a simulation capped and continued by a second process leaves the same `events.jsonl` as an uninterrupted one. A machine rebuilt from the journals alone does not.
- `recorded_config` and `recorded_config_in` return the configuration of the latest `config.loaded`, standing in for the file `togi run` would load.

The tuner's simulation tests live under [`crates/togi-simrun/tests/sim/`](../crates/togi-simrun/tests/sim/), because they need whole simulated sessions. `togi-tuner` cannot take `togi-simrun` as a dev-dependency: that would build a second `togi-tuner` whose types do not unify with the first.

### The simulator command (`togi-tool-sim`)

The source-checkout command above runs the developer script on `Persistence::Files`. A session exits 0 for a stop other than a dead end, 1 for an error or a dead end (`sim: dead end CONDITION: DETAIL`, then one `evidence:` line per cited event), 2 for usage errors and 3 when the boot cap ends an unfinished session. SIGINT, SIGTERM and SIGHUP cancel the session through its normal cleanup path.

The journal records the build version and revision, ruleset and evidence epoch. On continuation, a saved machine snapshot supplies the seed; otherwise `--seed` applies to the fresh machine built from history.

### Bounded comparisons

`just same BASE` runs the same five named scripts in both trees: `mapping-stop` (credited mapping, computation error and signal stop), `mce-resume` (uncorrected load/store machine check and successful resumed work), `power-button-reset` (an in-flight reset and resumed work), `profile-placement` (staged stock hold, real qualification and verified day application, then an exact-profile filler trial), and `no-nonzero-work` (dead end and stock restoration). `just smoke BASE` runs only `mapping-stop` and `mce-resume`. These compare normalized session journal bytes on the named paths, not tuning quality or physical hardware. Both trees must declare scripted input contract v1, and both simulators must affirm it. Older bases without the contract are refused, not counted as equal. See [Proving unchanged decisions](benchmarking.md#proving-unchanged-decisions).

## Reading real reference journals

[`just stats`](reviewing.md) reports exposure and failures from real journals. The [historical evidence and gate](https://github.com/shgew/togi/issues/105) record the archived reference sessions. The [archived trial corpus](https://github.com/shgew/togi/blob/archive/pre-cut/tools/bench/facts/target.jsonl.gz) is historical evidence, not a simulator input.

Trial hours are last-evidence exposure, not wall-clock session duration. A crash can have zero recorded exposure, and older journals can record a failure before `trial.start`. Rulesets, offsets, workloads and intended durations vary between sessions, so pooled rates are not per-offset failure probabilities.
