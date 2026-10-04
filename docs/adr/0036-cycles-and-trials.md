# Cycles and trials

[ADR 0034](0034-one-vocabulary-from-screen-to-journal.md) established one vocabulary from screen to journal. Two terms still obscure that vocabulary: a lap is one cycle through checking, and a start names the same observation already called a trial. The dashboard work in [#310](https://github.com/shgew/togi/issues/310) needs those concepts to have one name at every boundary.

## Decision

Call one pass through the checking schedule a **cycle**. Call one launch of one workload under a fixed profile a **trial**, the unit of pass evidence because failures can cluster at onset. Merge the glossary's Start entry into Trial. Neither definition nor any tuning decision changes. ADR 0034 remains in force except for the names amended here.

| Old term | New term |
|---|---|
| lap | cycle |
| full lap | full cycle |
| clean lap | clean cycle |
| passed lap | passed cycle |
| start, unit of pass evidence | trial |
| short / long start | short / long trial |

Wire kinds and fields follow the same vocabulary:

| Where | Old | New |
|---|---|---|
| Event kind | `checking.lap` | `checking.cycle` |
| Cycle, step and trial payloads; state | `lap` | `cycle` |
| State | `lap_open` | `cycle_open` |
| State and clean-cycle metrics | `clean_laps`, `last_clean_lap` | `clean_cycles`, `last_clean_cycle` |
| Passed-cycle metrics | `passed_laps` | `passed_cycles` |
| Shutdown count and stop reason | `laps` | `cycles` |
| Hunt payloads and evidence state | `starts` | `trials` |
| Hunt short-trial duration | `start_s` | `trial_s` |

Other Go identifiers, fields, messages and metrics built from these terms follow the same mapping: for example, `CheckingLap`, `LapEvent` and `LapStart` become `CheckingCycle`, `CycleEvent` and `CycleStart`. The cycle event enum value `start` remains unchanged: it names a cycle beginning, not a unit of pass evidence.

| Setting | Old | New |
|---|---|---|
| Configured checking schedule | `[checking] lap = [...]` | `[checking] cycle = [...]` |
| Short-trial duration | `durations.start_s` | `durations.short_trial_s` |
| Recorded effective configuration | `config.loaded.config.checking.lap`, `config.loaded.config.durations.start_s` | `config.loaded.config.checking.cycle`, `config.loaded.config.durations.short_trial_s` |
| CLI | `togi run --laps N` | `togi run --cycles N` |
| Development simulator | `tools/sim --laps N` | `tools/sim --cycles N` |

`short_trial_s` remains the duration of hunt-group trials, reruns, deepening checks and R7's short trials. There are no aliases for the old configuration keys or flags. Keep the verb start and names for things beginning: `session.start`, `trial.start`, a session start, service starts, `start_offsets`, workload/process start times and CPU/SMU terminology.

## Considered Options

- **Rename only the dashboard:** rejected. It breaks ADR 0034's one-vocabulary rule and requires an operator to translate between the screen and its evidence.
- **Keep start as a second name for a trial:** rejected. It gives the same launch two names and obscures which unit a required pass count measures.
- **Retain old wire names or configuration aliases:** rejected. Current events and settings would preserve the vocabulary this decision removes.

## Consequences

`journal.Schema` increases from 3 to 4. This is breaking: the next run archives the older live session without appending to it, then starts a new session carrying eligible candidate solo limits, failure points and facts. The carry rules are unchanged: compatible same-BIOS trial facts retain their provenance, passes require the current evidence epoch, a BIOS change carries candidate solo limits only, and combinations and clean-cycle coverage do not carry. `tuner.Ruleset` and the evidence epoch do not change.

Archive, history, carry and replay readers translate schemas 1–3 into schema-4 names on read, chaining the schema-1/2 translations from ADR 0034. They never rewrite archived bytes. Replay continues accepting older schemas of the current ruleset. Committed journals remain byte-identical historical evidence.

Operators update explicit configuration keys and use `--cycles` before running the new build. Current specs, help, tools, dashboard strings and documentation use cycles and trials. ADRs 0001–0034 and released changelog entries retain their original words; these tables translate them. Simulated tuning decisions, solo limits, failure points and evidence facts remain unchanged.
