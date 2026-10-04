# Simulating

`tools/sim` runs a whole tuning session on the seeded simulator in `internal/sim`: a 16-core Zen 5 machine with hidden per-core limits, random failures and crashes. It needs no hardware and no root, and works on every development platform. It is a development program: the package does not ship it, so it runs from a source checkout.

```sh
just sim [seed]                                             # search, deepening and one clean lap in a new temporary state directory
go run ./tools/sim [--seed N] [--machine FILE] [--replay-facts] [--laps N] [--state-dir DIR] [--samples]
```

- `--seed` (default 1) selects deterministic limits and failures; the same seed and history reproduce the journal.
- `--machine FILE` loads an explicit simulator machine TOML, including limits, joints, ranking, outcome scripts and reset reasons; `--seed` still sets its seed.
- `--replay-facts` answers exact trial-class/full-profile matches from the machine file's `facts` extract, under its declared BIOS context. Without this flag the file remains a fitted simulator alone.
- `--laps` (default 1) stops after N clean laps valid for the current profile once every core is at its limit and deepening can reach no more depth. An earlier lap can count after a deepening under the uncontradicted-profile rules in [the tuner spec](spec/tuner.md#checking).
- `--state-dir` uses an existing directory; without it, `sim` creates a temporary one and prints its path to stderr.
- `--samples` writes `trials/<trial-id>/samples.jsonl` for later inspection; by default trial samples stay in memory.

A crash reboots the simulated machine in-process and the next boot resumes the journal, as a real reboot would. Within one invocation, parsed events stay in memory across simulated reboots; `events.jsonl` is still appended on every event, but `state.json` is written only when the invocation stops. Journal lines go to stderr as `togi run` logs them, and nothing is fsynced. The state-directory writer lock stays held across simulated reboots. Read-only commands can inspect the final state after the invocation returns; during a run, `state.json` can be absent or still describe the previous invocation.

A state directory that already holds a journal or archives resumes the simulated machine after them: boot numbering continues and the clock starts after the last event, so a crash in the new run is never mistaken for an old boot, and a session after `reset --all` gets a new id. A new invocation reads the file-backed journal before tuning; only reboots within that invocation reuse the parsed events. Real `togi run` sessions retain their file-backed recovery and per-event state writes.

Simulated trials keep samples in memory by default: each loaded worker's cumulative CPU milliseconds advance once per simulated second. The simulator retains only the last trial's specification, duration and stall metadata and generates its samples lazily when read. With `--samples`, trials instead write `trials/<trial-id>/samples.jsonl`, encoding samples as they are generated without retaining the series. On a crash the loaded culprit's worker stops first, two seconds before the reset when the trial ran long enough, so the recovered `trial.end` can demonstrate `stalled_core` and `worker_stalled_ms` outside R6, which omits stalled-worker evidence because its workers are suspended by design. Samples in memory last one invocation, across its simulated reboots: when a new invocation closes a trial an interrupted one left open, its `trial.end` carries sample evidence only if both invocations ran with `--samples`. Otherwise both paths produce the same journal evidence; these synthetic samples are diagnostic only and do not change failure draws or tuner decisions.

The simulator reports no SMU `pm_table` lanes: `pm_table` is absent from samples, and each run records an informational `preflight.check` explaining that the version is unavailable and no per-core lanes are supplied.

The session uses the default configuration, never `/etc/togi/config.toml`, and runs unattended: an unanswered too-aggressive defect is a dead end. `sim` exits 0 when the session stops cleanly, 1 at a dead end or on an error, and 2 on a flag error. A journal written under an older ruleset or schema is archived and seeds a new session, as `togi run` does; the resumed machine reports the BIOS context the journals recorded, so their failure points carry. A journal written under a newer ruleset or schema is refused before another event is appended.

The read-only commands work on the result:

```sh
go run ./cmd/togi --state-dir <dir> status
go run ./cmd/togi --state-dir <dir> events --core 3
go run ./cmd/togi --state-dir <dir> watch
```

`status` reports clean laps since the last deepening (the count and latest lap number), valid per-workload starts and missing full-lap coverage. Its Tctl peak comes from passes together since the last profile change and names the source trial-end event.

A finished simulation's `watch` shows only its last moment. `just replay --state-dir <dir>` plays the whole journal through the dashboard on a simulated clock, 300 simulated seconds per second by default (`--speed`, which must be finite and positive), starting at `--from SEQ`; the dashboard's keys work as in `watch`. `--at SEQ` prints one frame as of that event instead, `--after 40s` that long after it, `--view help` or `--view log` for those views, with `--width`, `--height` and `--color` as for a frame on a terminal. It also plays a copied real journal.

`watch` and replay read the journal, not the sample files, so both work without `--samples`. Use that flag when inspecting the per-second sample series on disk.

Replay accepts all shipped journal schemas from the current ruleset, including schemas 1 and 2. Recordings from another ruleset are refused; replay needs the tuning rules used to write the journal. It translates older payload vocabulary while retaining recorded messages and configuration, so the dashboard uses the recorded checking lap schedule. An incomplete final line is ignored; replay never changes the recording.

Playback spaces consecutive events by their recorded boot-local `mono_ms` when both stamps are present and their boot IDs match; zero is a valid stamp. Across boots or with a missing stamp (including older journals), it uses the nonnegative wall-clock interval instead. The dashboard clock follows each consumed event's recorded `time`, then advances until the next event, so recorded wall-clock corrections remain visible without shortening or extending same-boot playback intervals.

Fault injection, explicit limits and the failure model are a Go API for tests (`sim.Config`, `sim.Limits`, `sim.Model` and the methods on `sim.Machine`); `internal/sim/doc.go` describes the model. `Machine.Hazard` returns the steady-state failure rate a trial would see at a given profile, from the same rules that draw trial failures, so a tool can judge a final profile against the model's truth. `internal/simrun` drives a session on the simulator across its crashes for tests that need a simulated journal. Its journal remains file-backed by default for recovery and interruption tests, and `Input.WriteSamples` opts into sample files. `tools/sim` opts into the in-memory journal path, which is checked against byte-identical journal and final-state files for fixed seeds on the default machine and `target-fit-0.toml`.

If a machine file sets `[model.signals]`, it replaces the default signal weights. Weights must be non-negative and sum to a positive total; an empty or all-zero map is rejected before the session starts.

`[ccd]` opts into an additional smooth R7 hazard for each loaded CCD. The rate in failures/second is `exp(log_rate + effect[ccd] + slope*(mean applied CCD depth-25))`; `effect` contains the two CCD log-rate effects, and mean depth includes every core's applied offset on that CCD, including zeros. A CCD contributes nothing when none of its cores are loaded. Its failures are unattributed crashes without core-local MCE evidence. All parameters must be finite and slope nonnegative. Existing core and joint hazards still contribute; files without this table keep the old model exactly.

If an active joint has any member on a loaded CCD, that CCD's smooth hazard is suppressed: joint explanations take precedence over extrapolation. A joint on the other CCD does not suppress this CCD's residual hazard.

Each `[[core]]` table uses `alone` for its five R1–R5 limits and `together` for its seven R1–R7 limits. In `[model]`, `past_limit_rate` is the failure rate one count past a limit, `growth` scales the rate for each additional count, and `near_limit_rate` is the loaded-core rate at or shallower than the limit.

A machine file sets the core count, BIOS context, ranking, model parameters, per-core limits, joints and scripted outcomes; unset keys keep the seeded defaults, and an unknown key is an error. If it specifies any per-core limits, it must provide a `[[core]]` table for every core. Set `[bios_context]` with `bios_version`, `board`, `cpu_model`, `microcode` and `boost_limit_mhz` to override the simulator's BIOS context. This one adds a pair that crashes only when cores 03 and 11 are both at −30 or deeper under R7:

```toml
cores = 16

[model.signals]
crash = 1

[[joint]]
members = { "3" = -30, "11" = -30 }
regimes = ["R7"]
```

A joint-triggered crash produces no MCE by default, even when `[model] crash_mce` enables MCEs for per-core crashes. To deliberately mislead attribution, add `crash_mce_core = 3` to the `[[joint]]` table: each crash from that joint leaves an uncorrected load-store MCE naming core 03 in the next boot. The named core must exist, but need not be a joint member or loaded. This explicit evidence takes precedence over an unattributed crash and can produce a single-core failure point instead of a combination; scenarios using it must state that expected attribution.

Run it with `go run ./tools/sim --machine <file> --state-dir <dir>` and inspect `events --kind hunt,combination,deepening` along with `status` and `watch`. `sim.Config` also models late-onset hazards (six minutes of R6 idle or four minutes of R7 heat soak), a flat rare hazard, a failure on an idle core, a backend error just before a crash, and watchdog, power-loss and thermal-trip resets. A scripted outcome pins a particular trial and time for deterministic interruption tests. The simulator has separate wall and boot-local monotonic clocks, so a wall-clock jump need not change which MCE belongs to a trial.

## Replaying real answers

```sh
go run ./tools/sim --seed 1 --machine tools/bench/machines/target-fit-0.toml --replay-facts --state-dir <dir>
```

The extract path in `facts` is relative to the machine file, and replay requires a declared BIOS context. `sim.NewReplay` and `sim.Config.Replay` are the simulator seam; the shared evaluation reader supplies decisive trial records, never idle facts. Matching uses regime, workload, sorted loaded cores, intended duration and every applied offset, including unloaded cores. It deliberately ignores condition and phase. Only facts from the declared BIOS context participate. A match draws uniformly from all matching records, deterministically from seed and trial ID/index, preserving the recorded outcome, failure signal and duration. Replayed failures occur at that recorded duration; after a crash it is only the last-evidence lower bound and can be zero, not a measured time to failure. A pass runs to the intended duration.

Non-matching trials and trial-less failures still come from the fitted machine underneath. The privacy-safe extract does not record the failing core or MCE bank: replayed backend failures name the first sorted loaded core, replayed crashes stay unattributed, and MCE signals use simulated bank evidence. These attribution details are not replayed hardware facts. The tuner receives ordinary journal evidence only; it cannot inspect the oracle. `Machine.Hazard` and `Machine.FailureProbability` continue to describe the fitted fallback. The [bench target scenario](benchmarking.md#the-suite) runs this oracle over the fitted ensemble and reports the share of trial outcomes supplied by real facts.

Replayed crashes record progress at their recorded exposure and use a simulated watchdog reset, independent of the fallback reset distribution, so recovery keeps their decisive failure. Replayed uncorrected machine checks record the signal before a simulated sync-flood reset; their core attribution still comes from simulated MCE bank evidence, not a backend-instance core.

## Measured reference journals

[`just stats`](reviewing.md) reports exposure and failures from real journals; it does not fit simulator defaults. These three archived sessions came from one Ryzen 9 9950X3D2 under one BIOS context. The session IDs identify the unmodified compressed fixtures in `internal/carry/testdata/`.

|Session (ruleset)|Regime|Starts|Trial hours|Crashes|Computation errors|Unexpected exits|
|---|---|---:|---:|---:|---:|---:|
|20260924T204352Z (1)|R1|388|12.854|15|0|1|
|20260924T204352Z (1)|R2|271|8.109|13|14|0|
|20260924T204352Z (1)|R3|54|2.900|0|0|0|
|20260924T204352Z (1)|R4|54|2.900|0|0|0|
|20260924T204352Z (1)|R5|22|1.572|4|1|1|
|20260924T204352Z (1)|R6|2|0.500|0|0|0|
|20260924T204352Z (1)|R7|2|0.008|2|0|0|
|20260926T151414Z (2)|R1|197|11.394|1|0|0|
|20260926T151414Z (2)|R2|155|8.395|9|8|0|
|20260926T151414Z (2)|R3|57|2.776|0|0|0|
|20260926T151414Z (2)|R4|50|2.567|0|0|0|
|20260926T151414Z (2)|R5|18|1.415|2|0|0|
|20260926T151414Z (2)|R6|3|0.750|0|0|0|
|20260926T151414Z (2)|R7|2|0.000|1|1|0|
|20260927T221954Z (3)|R1|51|4.250|0|0|0|
|20260927T221954Z (3)|R2|282|11.838|1|3|0|
|20260927T221954Z (3)|R3|17|1.417|0|0|0|
|20260927T221954Z (3)|R4|17|1.417|0|0|0|
|20260927T221954Z (3)|R5|17|1.378|0|1|0|
|20260927T221954Z (3)|R6|0|0.000|0|0|0|
|20260927T221954Z (3)|R7|20|0.701|9|4|0|

R2's failures concentrate in mprime AVX-512 (16, 15 and 4 respectively), with y-cruncher FFTv4/N63/VT3 contributing 11, 2 and 0. R7 mprime AVX2 contributes 1, 1 and 5, mprime AVX-512 1, 1 and 4, and y-cruncher 0, 0 and 4. Unattributed crashes load CCD0 alone 3, 1 and 7 times, both CCDs 2, 0 and 1 times, and CCD1 alone 0, 0 and 1 times. Failures contradicting earlier passes of the same class at equal-or-deeper profiles number 0, 1 and 4.

These are observed starts and last-evidence trial hours, not wall-clock session duration. A crash can have zero recorded exposure, and older journals can record a failure before `trial.start`. Rulesets, offsets, workloads and intended durations changed between sessions, so pooled rates are not per-offset failure probabilities. The simulator's fast seeded default remains unchanged. The synthetic bench machines are adversarial scenarios; the `target-fit-*` ensemble instead fits the committed extract and is checked against its eligible groups, with the limits described in [benchmarking](benchmarking.md#fitting-the-target-machine). The tables provide no evidence for adding R3 or R4 schedule-dependent hazards.
