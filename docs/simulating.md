# Simulating

`togi-tool-sim` (`just sim`) runs a whole tuning session on the seeded simulator of the `togi-sim` crate: a 16-core Zen 5 machine with hidden per-core limits, random failures and crashes. It needs no hardware and no root, and works on every development platform. It is a development program: the package does not ship it, so it runs from a source checkout.

The simulator is a model, not the machine. Research that runs it follows [Research campaigns](benchmarking.md#research-campaigns): model development changes the model under the [model program](../tools/fit/program.md), every other mode keeps the simulator frozen, and a simulated gain never lifts the tuner program's [evidence volume](../tools/bench/program.md#hard-rules) freeze. A simulated night ends where a real one does: when the operator ends it (`--night-hours`) or a dead end stops it.

```sh
just sim [--seed N] [--machine FILE] [--replay-facts] [--night-hours N] [--state-dir DIR] [--samples] [--max-boots N] [--verify-every N] [--cold-boots]
```

- `--seed` (default 1) selects deterministic limits and failures; the same seed and history reproduce the journal.
- `--machine FILE` loads an explicit simulator machine JSON file, including limits, joints, ranking, outcome scripts and reset reasons; `--seed` still sets its seed.
- `--replay-facts` answers exact trial-class/full-profile matches from the machine file's `facts` extract, under its declared BIOS context. Without this flag the file remains a fitted simulator alone.
- `--night-hours` (default 8) is when the simulated operator ends the night, counted in simulated time from the machine's clock when the command starts; a night has no other deadline. The operator's end is a signal, seen at the first event appended at or after that time (a trial in flight finishes first).
- `--state-dir` uses an existing directory; without it, `togi-tool-sim` creates a temporary `togi-sim-*` directory, keeps it and announces it on stderr as `sim: state directory DIR`.
- `--samples` writes `trials/<trial-id>/samples.jsonl` for later inspection, gzip-compressed on trial end; by default trial samples stay in memory. Simulation output, including compression, is never synced ([Journal, simulated storage](spec/journal.md#rules)).
- `--max-boots` (default 1000) caps the simulated boots of one invocation. A session still running after N boots stops with its journal and `state.json` as they stand, the reason on stderr, and exit 3.
- `--verify-every` (default 0) replays the whole journal into fresh state after every Nth simulated crash, and fails unless it equals the state the next boot resumes from; 0 checks only when the invocation ends: the session stops, an error ends it or it reaches `--max-boots`.
- `--cold-boots` replays the whole journal at every boot, as `togi run` does, instead of resuming from the state the crashed boot folded.

A crash reboots the simulated machine in-process and the next boot resumes the journal, as a real reboot would, from the state the crashed boot folded rather than a replay of the whole journal ([Journal](spec/journal.md#rules) lists what still runs at every boot and how the state is verified). Within one invocation, a warm reboot (the default) keeps the crashed boot's open journal, with its parsed events, so the next boot reads nothing back from `events.jsonl`, in memory and on disk alike; a cold reboot closes the journal and reads the file again. `events.jsonl` is still appended on every event. `state.json` stays pending at a warm crash and is written when the invocation stops or reaches its boot cap; it is written earlier only when a warm boot cannot keep the journal (the flush at its reboot failed, or the folded state does not cover the journal) and when an error ends the invocation, and a cold reboot writes it at every crash (the [reference section](#the-simulation-runner-togi-simrun) has the details). Journal lines go to stderr as `togi run` logs them, and nothing is fsynced. When stderr is not a terminal, both the log and the appended journal lines are buffered (64 KiB each) and written at exit, including on SIGINT, SIGTERM and SIGHUP, so a killed process loses its last unwritten lines and `events.jsonl` lags the run; on a terminal the log is written line by line. The state-directory writer lock stays held across simulated reboots. Read-only commands can inspect the final state after the invocation returns; during a run, `state.json` can be absent or still describe the previous invocation.

A state directory that already holds a journal or archives resumes the simulated machine after them: boot numbering continues and the clock starts after the last event, so a crash in the new run is never mistaken for an old boot, and a session after `reset --all` gets a new id. A new invocation reads the file-backed journal before tuning; only reboots within that invocation reuse the parsed events. Real `togi run` sessions retain their file-backed recovery and per-event state writes.

Unrepresentable resumed timestamps, elapsed wall times and next boot numbers are refused with operation-specific errors. Only absent history uses the default epoch; arithmetic failure never resets the clock or wraps a boot identity. Clock advances, wall-clock jumps and reboots leave the previous machine state unchanged when refused.

Simulated trials keep samples in memory by default: each loaded worker's cumulative CPU milliseconds advance once per simulated second. The simulator retains only the last trial's specification, duration, stall metadata and (when enabled) fixed-size voltage/clock snapshot, generating its samples lazily when read. With `--samples`, trials instead write `trials/<trial-id>/samples.jsonl`, encoding samples as they are generated without retaining the series. On a crash the loaded culprit's worker stops first, two seconds before the reset when the trial ran long enough, so the recovered `trial.end` can demonstrate `stalled_core` and `worker_stalled_ms` outside R6, which omits stalled-worker evidence because its workers are suspended by design. Samples in memory last one invocation, across its simulated reboots: when a new invocation closes a trial an interrupted one left open, its `trial.end` carries sample evidence only if both invocations ran with `--samples`. Otherwise both paths produce the same journal evidence.

Without `shared_voltage`, the simulator reports no SMU `pm_table` lanes: samples omit `pm_table`, and an informational `preflight.check` explains that the version is unavailable. With that object, every sample includes all 16 voltage requests and the loaded cores' clocks. Preflight identifies these as simulated lanes. The session summarizes them using the same warmup and minimum-sample rules as hardware; reading telemetry does not change failure draws.

These synthetic samples retain loaded-core clocks only: the model has no idle-clock or core/SoC power split. Hardware samples cover every core's clock and package RAPL power; all-core power is available from the PM-table lanes ([Conditions sampling](spec/workloads.md#conditions-sampling)). This does not change simulated telemetry or seeded decisions.

A fresh state directory uses the default configuration, never `/etc/togi/config.json`. A state directory with a journal resumes under the configuration its latest `config.loaded` recorded: the backend store paths and dead-end settings, so passes recorded under a real machine's backend store paths keep counting. That snapshot stands in for the configuration file the real machine would load: `togi run` loads its configuration file on every start and records it in a new `config.loaded`, never reading it back from the journal, so a simulated resume matches the next real run only while that file is unchanged since the recorded `config.loaded`. The simulator names no backend packages of its own, so a fresh session records none. The session runs unattended: an unanswered too-aggressive defect is a dead end. `sim` exits 0 when the simulated operator ends the night, 1 at a dead end or on an error, 2 on a flag error and 3 at the boot cap. A journal written under an older ruleset or schema is archived and seeds a fresh session that imports only raw day records, as `togi run` does; nothing tuning-related carries ([Transitions](spec/journal.md#transitions)). A live journal of an older schema keeps only its recorded backend store paths, on top of the default configuration, because the reader for older schemas keeps nothing else of a `config.loaded`. A journal written under a newer ruleset or schema is refused before another event is appended.

The read-only commands work on the result:

```sh
just togi --state-dir <dir> status
just togi --state-dir <dir> events --core 3
just togi --state-dir <dir> watch
```

`status` reports each CCD's D, S, slope and edge U, the covered and uncovered classes, the day policy, the night's queue shares and the Tctl peak ([Commands](spec/runtime.md#commands)).

A finished simulation's `watch` shows only its last moment. `just replay --state-dir <dir>` plays the whole journal through the dashboard on a simulated clock, 300 simulated seconds per second by default (`--speed`, which must be finite and positive), starting at `--from SEQ`; the dashboard's keys work as in `watch`. `--at SEQ` prints one frame as of that event instead, `--after 40s` that long after it, `--view help` or `--view log` for those views, with `--width`, `--height` and `--color` as for a frame on a terminal. It also plays a copied real journal.

`watch` and replay read the journal, not the sample files, so both work without `--samples`. Use that flag when inspecting the per-second sample series on disk.

Replay draws the same measured layout as `watch`: 240x67 has side-by-side CCD tables, a large trial countdown and context beside recent history; 160x45 uses an inline countdown and 120x33 compacts the lower panels. Use those sizes with `--at` to compare a recorded decision, night task or day step. Playback advances through R6 on its own clock; a live view instead holds its quiet frame until the journal records the trial's end.

Replay accepts all shipped journal schemas from the current ruleset, including schemas 1–4. Recordings from another ruleset are refused; replay needs the tuning rules used to write the journal. It translates older payload vocabulary while retaining recorded messages and configuration, so the dashboard shows the recorded night plan. An incomplete final line is ignored; replay never changes the recording.

Playback spaces consecutive events by their recorded boot-local `mono_ms` when both stamps are present and their boot IDs match; zero is a valid stamp. Across boots or with a missing stamp (including older journals), it uses the nonnegative wall-clock interval instead. The dashboard clock follows each consumed event's recorded `time`, then advances until the next event, so recorded wall-clock corrections remain visible without shortening or extending same-boot playback intervals.

Fault injection, explicit limits and the failure model are an API for tests: a `togi_sim::MachineDefinition`, the machine-file document, validated by `Model::new`, and the `Simulator` controls [below](#the-simulator-togi-sim). `Model::hazard` returns the steady-state failure rate a trial would see at a given profile, from the same rules that draw trial failures, so a tool can judge a final profile against the model's truth. `togi_simrun::simulate` drives a session on the simulator across its crashes for tests that need a simulated journal, in memory or on disk, and `write_samples` opts into sample files ([below](#the-simulation-runner-togi-simrun)). `togi-tool-sim` runs its session on disk with warm reboots; both persistence modes leave byte-identical `events.jsonl` and `state.json`.

If a machine file sets `model.signals`, it replaces the default signal weights. Weights must be non-negative and sum to a positive total; an empty or all-zero map is rejected before the session starts.

`model.regime_signals` sets signal weights per regime, under the same rules, for example an `"R7": {"crash": 79, "computation_error": 5}` member. A failure in a listed regime draws its signal from that regime's weights instead of `model.signals`. When the object lists any regime, a loaded joint without its own `signal` and a `ccd` failure, which otherwise crash, draw their signal too: from their regime's weights if listed, else from `model.signals`. A joint whose members are all unloaded still crashes. `just fit` writes both objects from the extract's recorded failure signals ([benchmarking](benchmarking.md#fitting-the-target-machine)). Files without `model.regime_signals` keep their exact previous behavior.

`ccd` opts into an additional smooth R7 hazard for each loaded CCD, which R9 loads take too. The rate in failures/second is `exp(log_rate + effect[ccd] + slope*(mean applied CCD depth-25))`; `effect` contains the two CCD log-rate effects, and mean depth includes every core's applied offset on that CCD, including zeros. A CCD contributes nothing when none of its cores are loaded. Its failures are unattributed crashes without core-local MCE evidence, unless `model.regime_signals` lists any regime: then they draw from the failing regime's weights, else from `model.signals`, and a computation error names the CCD's first loaded core. All parameters must be finite and slope nonnegative. Existing core and joint hazards still contribute; files without this object keep the old model exactly.

If an active joint has any member on a loaded CCD, that CCD's smooth hazard is suppressed: joint explanations take precedence over extrapolation. A joint on the other CCD does not suppress this CCD's residual hazard.

### Shared-voltage R7 model

`shared_voltage` opts into a common-rail model on a 16-core, two-CCD machine. It **replaces** all per-core limit/flat/idle hazards, `ccd` hazards and joints for multi-core R7 and R9 only; its only sources there are voltage margins and platform background. Unloaded cores' offsets cannot change that hazard. Single-core R7 and R9, R8 and R1–R6 retain their existing per-core mechanisms. Files without this object retain their exact previous behavior, including journals.

Here multi-core R7 means an R7 trial loading at least two cores; single-core R7 loads exactly one. It does not mean a failure attributed to one core within a multi-core load. The shared-rail model applies to a load of more than one core; the tuner's normal R7 partial chains stop before a one-core load ([workloads](spec/workloads.md#regimes)). R9 is modeled as R7 (see R8 and R9 below), so a multi-core R9 trial takes the rail too.

The default [target fitter](benchmarking.md#fitting-the-target-machine), its bootstrap refits and its forward-prefix fits all use this rail model for multi-core R7, with zero onset and no fitted joints or CCD residual. The committed `target-fit-*` files remain the legacy Go-generated ensemble: the [2026-10-17 frozen forward score failed](benchmarking.md#the-2026-10-17-frozen-score), so no machines, overlays, anchor or baseline were regenerated, and no Ruleset 12 gate was registered. The refreshed replay extract contains the nine scored sessions; it does not validate the fallback model.

```sh
just sim --seed 1 --machine tools/bench/machines/shared-voltage.json
```

That hand-set scenario is shaped after measured request spreads and clocks, not a fitted target-machine claim or a new bench baseline. Its CCD0 bases span 48 mV and CCD1 bases 30 mV; AVX-512 core 11 needs core 10 or 13 to supply a sufficient request on the example deep profile. y-cruncher uses the more demanding of each core's AVX2 and AVX-512 thresholds, pending hardware evidence.

The object supplies `idle_v`, `margin_v` (hazard smoothing width), `rate` (failures/second per core at its threshold), and `power_limit_w` and `thermal_limit_w`. Both limits are effective steady-state package-power budgets in watts; the smaller applies. This is a simple steady-state model, not a heat-soak simulation. All must be finite and positive. `idle_v`, and every loaded request across the offset and clock range, must also remain finite when converted to the recorded float32 voltage-request lanes.

Optional `background_rate` defaults to zero and must be finite and nonnegative. It adds one CO-independent platform-failure rate per multi-core R7 or R9 trial, in failures/second, independent of workload, profile (including all-zero offsets), and loaded-core count. It is separate from the voltage-margin hazards: drawing it always produces an unattributed crash, without MCEs or a fabricated stalled worker. The usual onset boost applies; draws, `Model::hazard` and `Model::failure_probability` use the same rate. It does not apply to single-core R7 or R9 loads or other regimes, and replayed outcomes bypass it. Omitting it preserves existing behavior.

For each of the three R7 workload IDs, `shared_voltage.workload.<id>` supplies:

| Key | Meaning |
|---|---|
| `reference_mhz` | Clock at which request bases and required-voltage thresholds are defined |
| `full_mhz` | Two unloaded-package full-CCD clock intercepts, MHz |
| `idle_gain_mhz` | Additional MHz per idled core on that CCD |
| `watts_per_core` | Loaded-core power at offset −35 |
| `offset_watts_per_count` | Additional watts per shallower count |
| `package_mhz_per_w` | Clock reduction per watt over the effective package budget |
| `balance_mhz_per_w` | Clock reduction per watt that CCD exceeds the mean CCD power; the cooler CCD receives the corresponding boost |

For a loaded CCD, clock is `full_mhz[ccd] + idle_gain_mhz*(8-loaded_count) - package_mhz_per_w*max(0, package_watts-budget)`. When both CCDs load, subtract `balance_mhz_per_w*(ccd_watts-package_watts/2)` too. Power sums `max(0, watts_per_core + offset_watts_per_count*(offset+35))` over loaded cores only. Round clocks to whole MHz, at least 1. This captures package pressure and cross-CCD power redistribution without a feedback solver. At −35 on the supplied machine, whole-CCD AVX2 runs 5.24/5.22 GHz and seven cores gain 30 MHz. All-core AVX-512 runs 4.799 GHz; setting CCD0 to offset 0 gives 4.329/4.889 GHz.

Each workload requires a `core` array of 16 objects in core-ID order at `shared_voltage.workload.<id>.core`. Each supplies `base_v` at offset 0, `threshold_v` at the reference clock, `count_v` (default 0.0036 V/count), and `clock_v_per_100mhz`. The optional `threshold_clock_v_per_100mhz` defaults to zero. Both clock coefficients must be finite and nonnegative; no upper bound is imposed by the machine decoder. Loaded request is `base_v + count_v*offset + clock_v_per_100mhz*(clock-reference_mhz)/100`. Idle requests equal `idle_v` and **never set the modeled rail**. The shared voltage is the maximum loaded request across both CCDs.

The 3.6 mV/count default is a modeling assumption, not a hardware-calibrated constant. It is `DEFAULT_COUNT_V` in `crates/togi-sim/src/definition.rs`. Explicit `count_v` values in a machine file replace the simulator default; fitted values are in-sample parameters.

Required voltage for a loaded core is `threshold_v + threshold_clock_v_per_100mhz*(clock-reference_mhz)/100`, using that core's CCD clock. This optional V/F term models the voltage needed to sustain a higher frequency, separately from the voltage the core requests. Equal request and required-voltage slopes cancel a common clock shift when the same CCD supplies the rail; a different CCD setting the rail need not cancel it. With the default zero coefficient, existing machines keep their clock-independent thresholds and exact previous behavior.

Each loaded core has margin `shared_voltage-required_voltage` and rate `rate*softplus(-margin/margin_v)/ln(2)`, where `softplus(x)=ln(1+exp(x))`. Hazards are smooth above and below the threshold, increase below it, and sum across loaded cores. The usual onset boost applies equally to draws and `Model::failure_probability`; `Model::hazard` returns the steady-state sum. `Model::r7_failure_share` exposes an individual loaded core's shared-voltage rate divided by the total steady-state hazard, including platform background, for attribution likelihoods. Background contributes no named-core numerator. The method returns zero for invalid or unloaded cores, trials outside multi-core shared-voltage R7 or R9, or zero total hazard.

A failing core draws from its optional `signals` map, otherwise from its regime's `model.regime_signals` weights (R7's for R9), otherwise `model.signals`. Only a computation-error draw remains a computation error naming that core; every other signal becomes an unattributed crash, without fabricated MCEs. Worker-stall telemetry can still name the core that stopped first. Fixed seed and history reproduce failures and telemetry. Samples of loads off the rail use the AVX2 request/clock parameters illustratively, without changing their hazards. Other PM-table arrays are synthetic: loaded C0 is 100%, idle CC6 is 100%, and unmodeled power and temperature lanes are zero.


Each object in the `core` array uses `alone` for its five R1–R5 limits and `together` for its seven R1–R7 limits. In `model`, `past_limit_rate` is the failure rate one count past a limit, `growth` scales the rate for each additional count, and `near_limit_rate` is the loaded-core rate at or shallower than the limit.

An optional `core[].workload` object overrides both limit arrays for its catalog workload IDs, with limits in −50…1. Unknown IDs are decode errors. The object is not scoped to one regime: a core may override workloads from any regime, and an override applies only when that exact workload runs.

**R8 and R9.** The simulator models both role regimes ([workloads](spec/workloads.md#regimes)) so that Ruleset 13's night mapping, qualification and control cells run on it. Each is modeled as the regime closest to its load shape, with no new machine-file field:

| Role regime | Modeled as | Why |
|---|---|---|
| R8 | R2 | A light role beside R2 workloads sustained on every other core of its CCD. |
| R9 | R7 | Every thread of every core, which loads the package like an all-core R7 trial. |

- **Limits.** A load reads the `together` slot of the regime it is modeled as on every loaded core, and the `alone` slot when it is alone (no other core at a nonzero offset). The `together` array keeps its seven R1–R7 entries. A `core[].workload` override naming an R8 or R9 catalog ID (for example `mprime-sse-4k-21k-neighbours`) replaces that for exactly that workload, so a machine file can make either stricter or looser than its model regime per core.
- **Signals.** A failure draws from `model.regime_signals.R8` (or `.R9`) when listed, else from the model regime's entry (`R2`, `R7`), else from `model.signals`. A machine file need not list R8 or R9. The shipped fitted machines (`target-fit-*`, `target-shared-voltage`) list R1, R2, R5 and R7 only, so their R8 failures follow their R2 mix and their R9 failures their R7 mix. Their bytes are unchanged, so their bench digests and cached sessions stay valid, and a session that never runs R8 or R9 is unaffected.
- **Joints.** `joint.regimes` may list `R8` and `R9`, and an empty list still means every regime. A joint also covers the regime a listed regime is modeled as: one that names `R7` covers R9, one that names `R2` covers R8. The shipped joints name R7, so they apply to R9.
- **R9 is R7 for the rail, `ccd` and background.** A multi-core R9 load uses the shared rail, the `ccd` residual and `background_rate` exactly as an R7 load of the same loaded set (its `cores`) does. Because no R9 workload is one of the three R7 workloads, the rail uses the first R7 workload's parameters (mprime AVX2) for every R9 workload; this is the same fallback that gives other regimes illustrative telemetry, and for R9 it also sets the hazard. A fitted rail does not model the lighter R1 load of R9, so an R9 hazard on a shared-voltage machine is an R7 hazard, not a fitted R9 claim.
- **R8 is not R7.** An R8 trial never takes the rail, the `ccd` residual or the background, even on a shared-voltage machine, and its samples use the first R7 workload's request parameters illustratively like other non-R7 trials.

`togi_sim::limit_index` names the limit slot a regime reads (R8 → R2's, R9 → R7's); `regime_index` is the position R1–R9 take in `regime_signals` and joint regime sets. Every regime is modeled, so neither returns an option and the simulator has no unmodeled-regime error. Draw keys of R1–R7 are unchanged, so every recorded stream of those regimes is too. The calendar day/night experiment ([below](#calendar-daynight-experiments)) does not use this model.

The `togi-sim` crate owns the machine-file format: `togi_sim::file::load` decodes and validates it, materializing overlays, and `togi_sim::file::encode` writes it; the differences from the plain JSON document are [below](#the-simulator-togi-sim). A run's seed, recorded history and replay facts are not machine-file fields: they are `RunSettings`. `togi-tool-fit` supplies only its provenance notes to the encoder.

A machine JSON file sets the core count, BIOS context, ranking, model parameters, per-core limits, joints and scripted outcomes; unset keys keep the seeded defaults, and an unknown member is an error. If it specifies any per-core limits, its `core` array must provide an object for every core. Set `bios_context` with `bios_version`, `board`, `cpu_model`, `microcode` and `boost_limit_mhz` to override the simulator's BIOS context. A hand-written machine's `description` string explains the scenario; generated fits use a `notes` array of strings for provenance and model-check qualifications. Neither metadata field changes simulated behavior. This one adds a pair that crashes only when cores 03 and 11 are both at −30 or deeper under R7:

```json
{
  "description": "Cores 03 and 11 crash together at offsets -30 or deeper under R7.",
  "cores": 16,
  "model": {"signals": {"crash": 1}},
  "joint": [
    {
      "members": {"3": -30, "11": -30},
      "regimes": ["R7"]
    }
  ]
}
```

A joint-triggered crash produces no MCE by default, even when `model.crash_mce` enables MCEs for per-core crashes. To deliberately mislead attribution of a failure to a core, add `"crash_mce_core": 3` to the object in the `joint` array: each crash from that joint leaves an uncorrected load-store MCE naming core 03 in the next boot. The named core must exist, but need not be a joint member or loaded. This explicit evidence takes precedence over an unattributed crash and is charged to the tested CCD, not to that core; scenarios using it must state the expected attribution.

Run it with `just sim --machine <file> --state-dir <dir>` and inspect `events --kind night.plan,day.step` along with `status` and `watch`. The machine file also models late-onset hazards (`onset_s` and `onset_boost` for the initial window, a joint's `after_s` for a delayed one: six minutes of R6 idle or four minutes of R7 heat soak), a flat rare hazard (`flat`), a failure on an idle core (`idle`), and watchdog, power-loss and thermal-trip resets (`model.reset`). A `script` entry pins a particular trial's signal and time, optionally followed by a crash (`then_crash`) or a reset kind (`reset`), for deterministic interruption tests. The simulator has separate wall and boot-local monotonic clocks, so a wall-clock jump need not change which MCE belongs to a trial.

### Machine overlays

A machine file that names a parent in `extends` is an overlay: a scenario that is a named mechanism on an existing machine. It decodes to the parent's decoded machine with only the listed `shared_voltage_override` entries changed, so everything else (limits, model parameters, signal mix, every other shared-voltage value) is the parent's and follows when the parent changes.

```json
{
  "description": "Overlay of target-shared-voltage.json: core 5's AVX2 required voltage rises faster with clock.",
  "extends": "target-shared-voltage.json",
  "shared_voltage_override": [
    {
      "workload": "mprime-avx2-36k-248k-allcore",
      "core": 5,
      "threshold_clock_v_per_100mhz": 0.06
    }
  ]
}
```

`extends` is a path relative to the overlay's own directory, never absolute; the parent may itself be an overlay, and a cycle is an error. Each override entry names a `workload` and a `core` of the parent's `shared_voltage` and sets `threshold_v`, `threshold_clock_v_per_100mhz` or both; an absent field keeps the parent's value, and the result must be a valid machine. An overlay sets no other machine field: only `description` and `notes` may sit beside `extends` and `shared_voltage_override`. An unknown workload or core, a repeated target, an entry that sets neither field and an empty list are errors, as is a parent without `shared_voltage`. The format has no general patching.

A parent's `facts` path is relative to the parent, so the overlay's decoded `facts` is rebased to resolve to the same extract from the overlay's directory. `togi_sim::file::load` returns the machine; `togi_sim::file::load_with_files` also returns every machine file it was decoded from, the overlay first and then its parents in turn (one path for an ordinary machine file, never the facts extract), for a caller that must notice when any of them changes. `togi_sim::file::encode` always writes the materialized machine, never an overlay.

## Replaying real answers

```sh
just sim --seed 1 --machine tools/bench/machines/target-fit-0.json --replay-facts --state-dir <dir>
```

The extract path in `facts` is relative to the machine file, and replay requires a declared BIOS context. `togi_sim::replay::Replay::new` and `RunSettings::replay` are the simulator seam; the shared evaluation reader, `togi_tool_trialfacts::Extracts::replay`, supplies decisive trial records, never idle facts. Matching uses regime, workload, sorted loaded cores, intended duration and every applied offset, including unloaded cores. It deliberately ignores condition and phase. Only facts from the declared BIOS context participate. A match draws uniformly from all matching records, deterministically from seed and trial ID/index, preserving the recorded outcome, failure signal and duration. Replayed failures occur at that recorded duration; after a crash it is only the last-evidence lower bound and can be zero, not a measured time to failure. A pass runs to the intended duration.

For example, a matching crash fact with `duration_s = 0` crashes at the simulated trial's start. It has zero trial exposure, then pays the simulated reboot time; replay does not spread such a failure uniformly through the intended trial or replace zero with a guessed duration.

Non-matching trials and trial-less failures still come from the fitted machine underneath. The privacy-safe extract records the `core` a backend signal named in a together trial's `trial.end`, but neither a core named only by an MCE nor the MCE bank, and replay does not use the recorded core: replayed backend failures name the first sorted loaded core, replayed crashes stay unattributed, and MCE signals use simulated bank evidence. These attribution details are not replayed hardware facts. The tuner receives ordinary journal evidence only; it cannot inspect the oracle. `Model::hazard` and `Model::failure_probability` continue to describe the fitted fallback. The [bench target scenario](benchmarking.md#the-suite) runs this oracle over the fitted ensemble and reports the share of trial outcomes supplied by real facts.

Replayed crashes record progress at their recorded exposure and use a simulated watchdog reset, independent of the fallback reset distribution, so recovery keeps their decisive failure. Replayed uncorrected machine checks record the signal before a simulated sync-flood reset; their core attribution still comes from simulated MCE bank evidence, not a backend-instance core.

### Failure timing

The simulator (`crates/togi-sim/src/trials.rs`) answers a trial in this order: a matching replay fact, a scripted outcome, then fitted-model draws. A replayed pass runs the full intended duration; a replayed failure uses the fact's recorded duration. This preserves the extract's exposure convention, not an inferred hardware crash time.

For the fitted fallback, every active failure source draws an exponential waiting time independently in its seeded RNG domain. The earliest draw before the intended end wins; if none occurs, the trial passes. These sources include loaded and unloaded cores, voltage margins, CCD residuals, joints and platform background where the machine enables them. A joint's `after_s` delays its hazard. During the initial `onset_s` window, `onset_boost` multiplies an active rate by `1 + onset_boost`; after that window the steady-state rate applies. The fitter does not estimate onset or delay from binary pass/failure counts ([fitting limits](benchmarking.md#fitting-the-target-machine)).

Computation errors, stalls, unexpected exits and crashes stop at the selected time. A fallback corrected MCE is logged at that time but lets the trial finish its intended duration; a replayed corrected MCE instead uses the recorded duration. An uncorrected MCE causes a reset at its selected time. Each crash reboot adds 90 simulated seconds outside trial exposure. `Model::hazard` reports steady-state risk without delays or onset boosts; `Model::failure_probability` integrates them over the intended duration. Neither uses replayed outcomes.

### The simulator (`togi-sim`)

`crates/togi-sim` is the simulator library: `Model::new(&MachineDefinition, seed)` validates a machine file's document, and a `Simulator` built from a `Model` and `RunSettings { seed, history, replay }` hands the session its seams (`Simulator::seams`, a `togi_machine::Machine`) and offers the controls tests use: `jump_wall`, `crash`, `next_reset`, `power_loss`, `reboot`, the fault arms (`fail_write`, `crash_at_write`, `crash_before_apply`, `corrupt_readback`, `fail_setup`, `miss_backend`, `escape`, `fail_check`), `script_trial`, `set_bios_context`, `set_samples_dir` and `interrupt_smu`, whose hook sees every SMU access before and after its effect. `History` (boots, last event time, BIOS context) continues a recorded run: a machine starts one reboot after the last event and numbers its boots after the recorded ones.

A journal does not hold everything the next boot observes. `Simulator::persisted` returns a serializable `Persisted` snapshot of the machine, and `Simulator::restore` continues the machine from it. The snapshot holds:

- the seed;
- the clocks;
- every boot with its reset reason and kernel log, as journald keeps them;
- the machine checks and the reset waiting for the next boot;
- the registers and BIOS context;
- the last trial's samples.

Draws are keyed by seed and typed parts, so no generator state is kept. Armed faults, added scripts and recorded violations are not kept. A snapshot decodes from objects only, as a machine file does, and a snapshot for another number of cores is `RestoreError::Cores`.

The machine-file format is the same document, with these differences:

- **Strict validation.** `Model::new` checks every number: finite, in range, `growth` positive, probabilities in 0 to 1, weights nonnegative with a positive total (reset weights too, so an empty reset map is an error, not a silent watchdog), `onset_boost` and `onset_s` nonnegative. It refuses a hazard that cannot be represented: a largest limit rate (the near-limit rate, or the past-limit rate grown to the deepest offset) that is not finite, a core whose `flat` plus that rate is not finite, a CCD exponent that is not finite or above 700, and a rail whose worst-case hazard is not finite. Hazard rates and failure counts are otherwise evaluated without overflowing intermediates, and growth powers and onset counts without underflowing ones, so every source of an accepted machine has a finite rate and scores and draws agree; other arithmetic, such as the rail's clock and power model, is plain floating point, so absurd coefficients give absurd clocks and rates near the smallest float may round to zero. Fields are checked in document order, nested ones too, and an error names the field (`joint[2].members.3`). Decoding comes first, so a document missing a required field (a core without `alone`) is a parse error even when its core count is also wrong.
- **A joint's `rate` is optional.** Absent, it is the model's `past_limit_rate`; an explicit `0` is a joint that never fails. A joint needs at least one member. A script's `at_s` must be finite and nonnegative. File-defined scripts and scripts installed through `Simulator::script_trial` accept only failure signals the fitted simulator can draw; uncorrected MCEs are replay-only.
- **Strict decoding.** Objects only (a positional array is not a machine), no unknown fields and no duplicate keys, in maps too. `count_v: 0` still selects the 3.6 mV default.
- **Encoding.** `file::encode` writes the materialized machine as pretty JSON with every field equal to its default left out, and refuses a machine `Model::new` refuses.
- **Overlays** keep their own `description` and `notes`; the rest is the parent's.

Randomness is `rand`'s `StdRng`, one stream per typed domain seeded by a fixed splitmix64 fold over the run seed, the domain and its key parts. A loaded core's draws are keyed by the core, regime, condition, its applied offset and the trial index, never the trial ID or another core's offset, so a retry reproduces its class outcome; an unloaded core's draws also name the loaded *set*, whatever its order. Telemetry has its own domains and consumes no failure draw. The loaded set is a `CoreSet`, so a joint's failure is attributed to its lowest loaded member and a CCD's to the lowest loaded core on that CCD, not the first in the caller's order. Seeds and streams differ from the Go simulator's, so a seed's limits and failures differ from those behind the Go-era measurements in [benchmarking](benchmarking.md#historical-gates).

A trial's failure is the earliest strictly before the intended end of the sources `Model::visit_sources` lists (loaded and idle cores, shared-rail cores, CCDs, joints, the background), and the same list gives `Model::hazard`, `Model::failure_probability` and `Model::r7_failure_share`, so scores and draws cannot diverge. Each trial runs once: a second `wait` is an error, also after a cancelled one, and a trial of an earlier boot is `Error::Crashed`. A crash keeps the exposure the trial ran before it, also for a script that crashes after its signal. An SMU hook that crashes or reboots the machine before an access makes that access `Error::Crashed` without applying it.

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

Both modes publish `state.json` only when a run ends, never after each event (`PublicationPolicy::OnFinish`). A run that stops, by its stop policy, a dead end or a signal, publishes it as its last step. A crash leaves it pending. With `Replay::Warm` the next boot adopts the capsule, which holds the folds the document would have been written from, so it neither reads nor rebuilds `state.json`, and the document is published once, from the last boot's capsule (`ResumeCapsule::publish`), when a run stops or `max_boots` caps the simulation, which leaves the last crashed run's. With `Replay::Cold` nothing adopts the capsule, so every crash publishes it and the next boot finds the document a live run would have left. A warm boot that cannot keep the journal, because its flush at the reboot failed or the capsule does not cover it, publishes the pending state before it reopens the directory, so the boot's check of `state.json` finds what it did before. An error that ends the simulation after a warm crash publishes the pending state before it returns. One default `togi-tool-sim --state-dir` session with 12 crashes renames `state.json` once with warm resumes and 13 times with cold ones.

Both modes leave byte-identical `events.jsonl` and `state.json`; a test pins this on three machines.

**Resume and verification.** `Replay::Warm` resumes each boot from the journal and the folds the crashed boot kept (`Journal::reboot`, `togi_session::prepare_kept`). `Replay::Cold` folds the whole journal again, as `togi run` does. Verification runs in both modes. It checks the capsule against a fresh fold of the journal (`ResumeCapsule::verify`) on these boots:

- every boot that ends the simulation, by a stop or by the cap;
- every `verify_every`th crashed boot.

A divergence is `SimError::Diverged`, and a state that cannot be published is `SimError::Publish`. A crashed boot that also failed in another way, such as a failed log write, is `SimError::Failed` with those errors. A log that cannot be flushed at the end is `SimError::FlushLog`. None of these is ever reported as a cap. In a run that stops, a failed log write is `SessionError::Log` among its errors.

**Observing the run.** An `observer` sees every appended event and may answer `Observe::Stop`, which cancels the run with `Stop::Signal`. It is called once per event, in order, at constant cost, and it keeps seeing events after the caller's log fails. It may also call the simulator's fault controls, such as `crash`, `next_reset` or `power_loss`. An observer works with either persistence. `write_samples` writes trial samples under the state directory's `trials/`, so it needs persistence on disk.

**After a run.** These functions read the state directory:

- `events` returns the live journal's events.
- `history` and `history_in` return a `togi_sim::History` that continues a resumed machine's boot numbering and clock.
- `resume_simulator` returns the simulator a later process continues with. A capped simulation leaves its machine as `machine.json` in the state directory, and the later process restores it. Without `machine.json`, it builds a fresh machine from `history`. A later `simulate` removes the file after its first boot that appends to the journal, so a continuation refused before it starts, by a configuration naming a missing core for example, leaves the machine for the next try. A test pins that a simulation capped and continued by a second process leaves the same `events.jsonl` and `state.json` as an uninterrupted one. A machine rebuilt from the journals alone does not.
- `recorded_config` and `recorded_config_in` return the configuration of the latest `config.loaded`, standing in for the file `togi run` would load.

The tuner's simulation tests live under [`crates/togi-simrun/tests/sim/`](../crates/togi-simrun/tests/sim/), because they need whole simulated sessions. `togi-tuner` cannot take `togi-simrun` as a dev-dependency: that would build a second `togi-tuner` whose types do not unify with the first.

### Calendar day/night experiments

`togi_sim::day_night` and `togi_simrun::day_night` are opt-in experimental modules.
They do not change the existing `Simulator`, machine-file model, or session runner.
`day_night::run(scenario, seed, PolicyKind, CalendarConfig)` builds a fresh CO-0, fixed-margin or Ruleset 13
adapter through `PolicyKind::build` and runs it; `run_with_policy` takes another implementation of the same `Policy`
interface. The two diagnostics are comparators. `PolicyKind::Ruleset13 { fallback_slope }` is the policy under test (the
gate role); its slope is the per-count hazard multiplier (1.5 or 2.0), so the policy's fallback slope is `beta = ln(multiplier)`.

The Ruleset 13 adapter (`Ruleset13`) runs the production `togi_tuner::policy::State`, not a second tuner. It keeps the journal
`togi run` and `togi day` would write as an in-memory list of events (envelopes with consecutive `seq`, a boot id per boot,
and timestamps from the calendar clock) and folds each appended event into the state. A firmware change appends a new
`session.context`. A day starts as a cold boot: `day.boot_start`, then `State::day` staged until it owes nothing (every
decision appended and folded), then the step's profile written through the SMU seam and `day.applied` with the verified
readback. A day interruption appends `day.checkpoint`, a successor boot's `day.boot_start`, `day.unclean_end` (the calendar has no reset
reason, machine check or kernel log, so they read as unknown) and `day.answer` (the operator's `cpu` answer is `frozen`,
`other` is `graphics`, no answer appends none). The adapter returns CPU-suspect exactly when the folded
`day.event` is CPU-suspect, definite or provisional. A night has no deadline: the adapter loops `State::next`,
appends each decision, and runs each trial plan through the machine `Trials` seam as the session does (whole profile
written and read back, `trial.intent`, `trial.start`, `trial.end` with the measured ran time).
The backend result is adjudicated by `togi_session::adjudicate_trial_end`, using the session's
pure evidence precedence without inventing kernel, reset, containment or sample observations.
The simulated operator ends each night when the calendar's night length runs out. A cell starts without asking whether it fits; a trial still
running at that moment runs only until then, and the runner returns it as cancelled. The adapter records it as the
session records a trial an operator's signal stopped: an inconclusive `trial.end` with `interrupted` and the reason
"trial ended before its full duration; stopped by signal". The night then closes with `shutdown{reason: signal}`
before the next day boot; a policy `EndNight` closes it with `shutdown{reason: dead_end}` instead. The adapter does not
apply day steps for a paused boot, because the calendar has no pause.
The calendar idealizes zero trial setup and teardown and applies no controller-placement exclusion or required repeat.
Its gate scores omit that overhead; actual overhead must be measured on hardware under the
[production workload rules](spec/workloads.md#durations-and-checking-schedule).
The registered benchmark and its gate reporting are described in [benchmarking](benchmarking.md).

The default calendar contains 730 days, with day zero on Monday, eight active daily hours,
and an allocation of eight testing hours on Monday, Wednesday and Friday. The allocation is
the simulated operator's night length: a trial still running when it runs out is stopped
there and returned as cancelled with the time it ran. A policy need not fill that
allocation: unused time and explicit clock sleeps are sleep, not exposure.
Actual daily-active hours, actual night-trial hours and sleep hours are reported separately.
Daily workload fractions can be overridden without changing the scenario's workload classes.
The built-in mix contains sustained two-hour blocks, not just short repeated tests.
Firmware transitions change the machine's BIOS context, clear its deployed offsets and
notify the policy; a calendar override can move the scenario's transition day.

The hidden truth is per CCD and full experimental class: workload catalog content, ordered
loaded cores and logical CPUs, process instances, roles, phase schedules, burst positions
and intended duration. Trial labels, indices and applied offsets are not class identity.
Daily activity and the night `Trials` adapter call the same truth model. Night requests are
ordinary `togi_machine::TrialSpec` values, including their actual placement and duration;
changing a probe's duration changes that class's identity. Only elapsed exposure before
the first competing event advances history. Positive waiting times are represented at a
minimum of one nanosecond, and events at the intended end do not terminate a trial.

CO hazard is excess over the same class at stock, exactly zero at CO 0; platform background
is a separate competing source. Exponential curves range from 1.2x to 6x per count.
The flattening family joins the central curve continuously to flatter tails.
The delayed-onset family needs 600 uninterrupted seconds of the same full class:
splitting identical exposure preserves that history, while changing class, interruption or
sleep resets onset. Thermal history heats toward 1800 heat-seconds with an 1800-second time
constant; its 600 heat-second threshold takes over twelve minutes from cold. Sleep cools
exponentially, so a short sleep preserves some heat, but never adds CO or background
exposure. Both-CCD interactions require simultaneous exposure of both CCDs.

The policy receives machine seams, ordinary night trial descriptions and daily completion
or interruption evidence, including a CPU/other/missing operator answer. It never receives
the scenario identity, true event cause, true hazard or safe profile. The non-CPU scenarios
include graphics and power interruptions, with correct, false CPU and missing answers.
Their hidden causes remain separate from the classifications returned by the policy;
CPU-suspect metrics count those actual returned decisions, not a runner-owned policy rule.
The fixed-margin adapter treats missing answers conservatively as CPU-suspect; a correct
other answer does not cause a retreat.

The fixed-margin comparator binary-searches an observed edge with probes of at least
half an hour, preserving longer supplied classes and their intended duration. It respects
the actual night budget and carries unfinished searches forward.
It tests each CCD while the other is at stock, restores daytime settings after probing,
then subtracts four counts from the observed edge. It approaches that fixed cap by at most
eight counts after each 50 actual daily-active hours; sleep and night trials cannot pay
that hold. A CPU-suspect interruption retreats deployed CCDs and their comparator caps by
one count. These deliberately simple decisions use observed trial results, not the oracle.

Only the reporting side computes a safe reference profile. It integrates the configured
ordered daily blocks and the remainder of a 24-hour cycle as sleep, including exact onset
and periodic-equilibrium thermal history, then divides by actual active hours. The default
reference is eight active hours plus sixteen asleep. It excludes policy-dependent nightly
stress, so a policy cannot change its target by changing its night schedule; actual daily
risk can still inherit heat from a preceding night. Background is excluded, per-CCD CO
excess is at most the joint target divided by CCD count, and total excess plus interaction
is at most the joint target.
The default joint target is one failure per 3000 active hours. Among feasible profiles,
the oracle maximizes total depth, minimizes CCD depth imbalance, then chooses the
lexicographically smaller depth vector (CCD 0 shallower on an exact tie). Safe profiles
are reused until the truth's cap epoch changes.

Reports retain every CCD: first convergence within one count of its safe reference,
maximum overshoot, promotion count, final and converged depth, and daily true CO crashes
both over the whole run and in year one. Climb crashes are counted until every CCD has
first converged. Joint crashes count for each affected CCD, while the unique daily CO
crash total counts the event once. Background-only, true-CO-justified and non-CPU-triggered
lost-depth counts remain separate; a retreat by eight counts records eight lost counts,
not one incident. Retreats of an unaffected CCD or without an event are unattributed, not
justified. Unconverged runs are right-censored with a missing convergence
day; they are never dropped from a benchmark denominator. Fixed-cap and moving-cap
reports remain distinguishable. Diagnostic comparator results cannot establish that the
registered real-policy gate passed.

### The simulator command (`togi-tool-sim`)

`crates/togi-tool-sim` builds the `togi-tool-sim` command. It runs from a source checkout:

```sh
just sim [--seed N] [--machine FILE] [--replay-facts] [--night-hours N] [--state-dir DIR] [--samples] [--max-boots N] [--verify-every N] [--cold-boots]
just sim-suite [--jobs N] [--max-boots N]   # the whole suite in one shard, then its check; on demand, not a CI check
cargo run --release -p togi-tool-sim -- suite --suite tools/bench/suite.json [--shards N] [--shard I] [--jobs N] --out DIR [--max-boots N]
cargo run --release -p togi-tool-sim -- check --suite tools/bench/suite.json --shards N [--shard I] DIR
```

A session exits 0 for a stop other than a dead end, 1 for an error or a dead end (`sim: dead end CONDITION: DETAIL`, then each cited event as an `  evidence: ` line), 2 for a usage error and 3 when `--max-boots` ends an unfinished session. A machine file and its replay facts are loaded before any state directory is created or locked, so invalid input leaves none. Without `--state-dir` a temporary `togi-sim-*` directory is created, kept and announced on the first line. A directory that holds a journal resumes under its recorded configuration, on the machine a capped invocation left in `machine.json` (whose seed wins over `--seed`), or else on a fresh machine after the recorded boots. The session runs on `Persistence::Files`. SIGINT, SIGTERM and SIGHUP cancel the session, which stops in order with `shutdown` reason `signal`. Everything, help included, goes to stderr: at once on a terminal, otherwise through a 64 KiB buffer, and a failed write or final flush of that log fails the command with status 1. It also:

- records the build: version and revision from `togi-version`, `togi_tuner::RULESET` and `togi_tuner::EVIDENCE_EPOCH`;
- refuses `--replay-facts` without `--machine` as needing a machine file, before any state;
- reports clap's diagnostics, and `--night-hours`, `--max-boots` and `--verify-every` refuse a negative number as an invalid value, and `--night-hours` refuses zero.

`suite` runs every session of one shard of the suite (`togi_tool_sim::shard`, equal cost per session) in this process, `--jobs` sessions at a time (default the available parallelism), each in its own in-memory state directory with warm resumes, the capsule verified after every crash and `--max-boots` (default 1000) as the cap. Sessions share nothing, so the manifest is the same for any `--jobs`; the full suite peaked at 416 MiB resident with `--jobs 4`. A session's night has no deadline: the simulated operator ends it after its scenario's `night_hours` (default 8; every scenario of `tools/bench/suite.json` sets 48) of simulated time, the first appended event at or after that time ending the night as a signal does. A session is `clean` only when its night ended the way its scenario expects (`night_end`: `operator`, the default, is that signal, the only end of a healthy night; `dead_end` expects a dead end the scenario's machine produces: a stock-unstable core at CO 0, a safety stop or every backend exhausted), its last boot's capsule matched a fresh fold of the journal, `togi_session::ReadState::replay` (the Ruleset 13 journal and policy fold) folds the whole journal and `togi-tool-audit` finds no violation in any journal of the directory or in its published `state.json`, which must exist, be at the journal's last event and be written exactly as this build writes the fresh fold. Otherwise it is `failed`, `capped`, `wrong_end` (the night ended another way than expected, and how), `verify_failed`, `replay_failed` or `audit_violations`, with its detail. Each session's line on stderr, in the order sessions finish, gives its id, its wall time and its outcome. The shard's results go to `DIR/shard-I.jsonl` in suite order, one JSON object per session, and the command exits 1 unless every session was clean. `check` reads a directory that must hold only `shard-I.jsonl` files and checks each shard's manifest against exactly the runs the suite's partition gives that shard, for every shard of the stated count or, with `--shard I`, for that shard alone: a shard with no manifest, a manifest that is not expected, a session missing, reported twice, reported by a shard that does not own it or not among those checked, and every session that was not clean are all named, and any of them exits 1. The suite is an on-demand research tool; no gate, pull request CI or release check runs it ([ADR 0087](adr/0087-simulations-are-research-tools-not-checks.md), [the session suite](benchmarking.md#the-session-suite)). `just sim-suite` runs it as one shard, then this check.

## Reading real reference journals

[`just stats`](reviewing.md) reports exposure and failures from real journals; it does not fit simulator defaults. The [historical evidence and gate](https://github.com/shgew/togi/issues/105) record the archived reference sessions. Their privacy-safe trial facts remain in `tools/bench/facts/target.jsonl.gz`.

Trial hours are last-evidence exposure, not wall-clock session duration. A crash can have zero recorded exposure, and older journals can record a failure before `trial.start`. Rulesets, offsets, workloads and intended durations vary between sessions, so pooled rates are not per-offset failure probabilities. The simulator's fast seeded default remains unchanged. Synthetic bench machines are adversarial scenarios; a fitted ensemble must pass its model check before supporting a target-machine claim ([benchmarking](benchmarking.md#fitting-the-target-machine)). These observations do not establish R3 or R4 schedule-dependent hazards.
