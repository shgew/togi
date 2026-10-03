# One vocabulary from screen to journal

The dashboard redesign in [#310](https://github.com/shgew/togi/issues/310) showed that words borrowed from the implementation read poorly to an operator. The screen began using plain descriptions while the code, configuration, journal and documentation still named the same concepts differently. An operator following a dashboard action into its journal needs one vocabulary, not a translation at each boundary.

## Decision

Use the vocabulary in `GLOSSARY.md` throughout the screen, CLI help, code, configuration, journal and current documentation. Definitions and evidence rules retain their meaning; this is not a change to tuning decisions. The mapping below translates the terms used by older ADRs and journals.

| Old term or value | New term or value |
|---|---|
| done, core state | at its limit; `at_limit` |
| resident, core state or phase | has room; `has_room` |
| isolated, trial condition | alone; `alone` |
| resident, trial condition | together; `together` |
| resident profile | the profile / the current profile |
| masked, trial condition | parked trial; `parked` |
| anchor | parked offsets; `parked` |
| mask, masks, hunt trial plan | group, groups |
| edge probe | member probe |
| edge, checked per-core offset | solo limit; `solo_limit` |
| candidate edge | candidate solo limit |
| edge, simulator's true failure boundary | limit |
| refinement, refine | deepening |
| guard | checking |
| rotation | lap |
| qualifying rotation | full lap; `full` |
| qualified rotation | clean lap |
| clean rotation, failure-free but not necessarily qualified | passed lap; `passed` |
| joint mark | combination; identifier prefix `C` instead of `J` |
| failed mark, mark of a core | failure point |
| carried mark | carried failure point |
| hunt, culprit, search, suspect | unchanged |

Wire kinds change with the concepts:

| Old kind | New kind |
|---|---|
| `guard.rotation` | `checking.lap` |
| `guard.step` | `checking.step` |
| `hunt.mask` | `hunt.group` |
| `mark.joint` | `combination` |
| `refine.round` | `deepening.round` |

Enums, payload fields, state keys and operator settings use the same names:

| Where | Old | New |
|---|---|---|
| `journal.Phase` | `resident`, `done`, `guard`, `refine` | `has_room`, `at_limit`, `checking`, `deepening` |
| `journal.Decision` | `check_edge` | `check_solo_limit` |
| `machine.Condition` | `isolated`, `resident`, `masked` | `alone`, `together`, `parked` |
| Shutdown and session stop reason | `rotations` | `laps` |
| `hunt.end.result` | `joint` | `combination` |
| `hunt.group.stage` | `edge` | `probe` |
| `checking.lap` | `rotation`, `clean`, `qualifying` | `lap`, `passed`, `full` |
| `checking.step` | `rotation` | `lap` |
| `trial.intent` | `rotation`, `mask` | `lap`, `group` |
| `shutdown` | `rotations` | `laps` |
| `hunt.start` and `deepening.round` | `anchor`, `anchor_seq` | `parked`, `parked_seq` |
| `hunt.group` | `mask`, `edge` | `group`, `probe` |
| `hunt.end` | `masks` | `groups` |
| `combination` | `mark` | `combination` |
| `session.carried` | `marks` | `failure_points` |
| `session.carried.carried[]` | `edge`, `edge_session`, `edge_seq` | `solo_limit`, `solo_limit_session`, `solo_limit_seq` |
| `session.carried.carried[]` | `failed_mark`, `mark_session`, `mark_seq`, `mark_signal` | `failure_point`, `failure_point_session`, `failure_point_seq`, `failure_point_signal` |
| `tuner.decision`, `core.phase` | `failed_mark` | `failure_point` |
| `core.phase` | `check_edge`, `cleared_joint` | `check_solo_limit`, `cleared_combination` |
| Configuration and `config.loaded.config` | `candidate_edges`, `guard` | `candidate_solo_limits`, `checking` |
| Configured durations | `guard_trial_s`, `guard_idle_s`, `guard_all_core_s` | `checking_trial_s`, `checking_idle_s`, `checking_all_core_s` |
| Configured schedule | `[guard] rotation = [...]` | `[checking] lap = [...]` |
| CLI | `togi run --rotations N` | `togi run --laps N` |
| `state.json` | `guard`, `joint_marks`, `refine` | `checking`, `combinations`, `deepening` |
| `state.json` | `rotation`, `rotation_open`, `qualifying` | `lap`, `lap_open`, `full` |
| `state.json` and metrics, qualified counts | `clean_rotations`, `last_qualified_rotation` | `clean_laps`, `last_clean_lap` |
| Metrics, merely failure-free counts | `clean_rotations` | `passed_laps` |
| `state.json` | `anchor`, `anchor_seq`, `masks`, `mask`, `edge`, `failed_mark`, `mark` | `parked`, `parked_seq`, `groups`, `group`, `probe`, `failure_point`, `combination` |

Other fields and messages built from these terms follow the same mapping. Names for unrelated concepts retain their meaning: CPU affinity bitmasks, record-only markers, completed-step counters and simulator shared-rail joints are not renamed by analogy. A parked trial retains the failed trial's workload and loaded cores, holds the group's cores at failing offsets and parks all others. A clean lap remains a passed full lap that ended with every core at its limit and is valid for the current profile; merely passing a lap is not enough.

## Considered Options

- **Rename only on screen, keeping code and journal words:** rejected. It leaves two vocabularies for the same action, so following evidence from the screen requires translation.
- **Keep wire names with a schema-free rename elsewhere:** rejected. The journal remains in words no current document uses, and decoded evidence still exposes the second vocabulary.

## Consequences

`journal.Schema` increases from 2 to 3. This is breaking: the next `togi run` archives an older live session without appending to it, then starts a new session carrying its eligible solo limits, failure points and facts. Archive and history readers continue accepting schemas 1 and 2 by translating their kinds, fields and enum values into the schema-3 names before decoding. They do not rewrite the archived bytes. `tuner.Ruleset` and the evidence epoch do not change for this rename.

Configuration keys and the lap-count CLI flag change as listed above, with no aliases for the old settings or flag. Operators using explicit settings update them before running the new build. Current specs, help, tool programs and unreleased changelog fragments use the new vocabulary. ADRs 0001–0033 and released changelog entries retain their original words; this decision's tables translate them without changing their historical record.
