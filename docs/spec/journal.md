# Journal and state

Normative rules for what shycler records. Goal: anyone, human or agent, reading only the journal can reconstruct everything shycler did and why, without reading code.

## Files

All files live in the state directory, default `/var/lib/shycler`:

| Path | Role |
|---|---|
| `events.jsonl` | The journal of the current session: one JSON event per line, append-only, source of truth |
| `state.json` | The current state, a projection of the journal for readers |
| `trials/<trial-id>/` | Per-trial work directory: backend config files, raw stdout/stderr, backend result files |
| `archive/<session-id>.jsonl` | Journals of sessions ended by `reset --all` |
| `lock` | Held with `flock` by the one process allowed to write the journal |

Retention: trial directories of failed and inconclusive trials are kept forever. Passing ones are pruned beyond the newest 200.

## Rules

1. **Everything is an event.** These are all events:
   - every action with an effect outside shycler's memory: SMU command, process start, signal or stop, scope creation, file written for a backend, sysctl, GRUB change;
   - every observation that feeds a decision;
   - every decision.

   The one exception: sub-second `SIGSTOP`/`SIGCONT` cycles in R3 and R4 are recorded as the schedule and seed at trial start and as counts at trial end.
2. **Intent before action.** An action that may crash the machine, such as an SMU write or a trial start, is appended and fsynced before it happens. On the next boot the last intent without a matching result is the in-flight action. Intents (`smu.intent`, `trial.intent`) are fsynced before `Append` returns, every event is written with a single `write`, and closing the journal fsyncs it. Opening the journal fsyncs the state directory and its parent, so a newly created journal file survives a crash. `run` appends `shutdown` when it stops on a signal, at a dead end, or on reaching the requested rotations; any other exit writes nothing, so a later boot whose journal ends without `shutdown` is treated as a crash (`workloads.md`, Failure signals). `run --sim` fsyncs nothing, because a simulated crash cannot lose written data.
3. **Decisions name their cause.** A decision event carries `cause`: the `seq` numbers of the events it was derived from, plus a `reason` in plain words.
4. **The journal wins.** On start, shycler rebuilds the state by replaying the journal. If `state.json` disagrees with the rebuild, it is rewritten and a `state.rebuilt` event records the difference.
5. **One writer.** `run`, `regain` and `reset` take `lock` before appending; `regain` and `reset` refuse while a `run` holds it. `status`, `cert` and `events` only read. `regain` and `reset --core` end with `shutdown` (reason `command`), so the next `run` never reads their boot as a crash. `reset --all` appends `command.reset` and `session.archived`, fsyncs, then moves `events.jsonl` to `archive/<session-id>.jsonl` and removes `state.json`; it refuses when that archive already exists. Opening a journal whose last event is `session.archived` finishes that move first and continues with an empty journal.
6. **Torn tails are expected.** A crash can leave a partial last line. Replay drops it and appends a `journal.torn` event with the discarded bytes, hex-encoded. A torn first line leaves an empty journal: nothing happened before `session.start`, so nothing is recorded.

## Event format

One JSON object per line. Common fields:

| Field | Meaning |
|---|---|
| `seq` | Integer, strictly increasing from 1 within a session |
| `time` | RFC 3339 UTC with nanoseconds, always nine fractional digits |
| `boot` | Kernel boot ID (`/proc/sys/kernel/random/boot_id`) |
| `kind` | Event kind from the catalog below |
| `msg` | One human-readable line, the exact text shycler logs |
| `cause` | Optional array of `seq` this event follows from |

Kind-specific fields are flat, snake_case and carry units in their names (`duration_s`, `period_ms`, `tctl_max_c`). Values use the vocabulary in `CONTEXT.md`. Cores are always `core` (the kernel `core_id`); logical CPUs are always `cpu`. The one exception to flat fields: `config.loaded` carries the effective configuration nested under `config`.

The first event of a session is `session.start` with `schema`, an integer bumped on any incompatible change.

Example trial, abbreviated:

```json
{"seq":812,"time":"2026-10-02T01:14:07.120000000Z","boot":"e8f9...","kind":"trial.intent","msg":"trial 0413 core 07 CO -32 R2 mprime AVX2 36K-248K 90s isolated","trial":"0413","core":7,"offset":-32,"regime":"R2","workload":"mprime-avx2-36k-248k","duration_s":90,"condition":"isolated","phase":"search","cause":[811]}
{"seq":815,"time":"2026-10-02T01:14:07.410000000Z","boot":"e8f9...","kind":"trial.start","msg":"trial 0413 started pid 48211 in scope shycler-trial-0413 on cpu 7","trial":"0413","scope":"shycler-trial-0413","pid":48211,"cpus":[7],"argv":["mprime","-t","-W/var/lib/shycler/trials/0413"],"cause":[812]}
{"seq":819,"time":"2026-10-02T01:15:37.460000000Z","boot":"e8f9...","kind":"trial.end","msg":"trial 0413 PASS 90s | Tctl max 71°C","trial":"0413","outcome":"pass","duration_s":90,"tctl_max_c":71,"cause":[815]}
{"seq":820,"time":"2026-10-02T01:15:37.461000000Z","boot":"e8f9...","kind":"tuner.decision","msg":"core 07 passed R1+R2 at -32; next -37 (coarse, no failed mark yet)","core":7,"phase":"search","decision":"step_deeper","from_offset":-32,"to_offset":-37,"pass":-32,"failed_mark":null,"reason":"coarse, no failed mark yet","cause":[811,819]}
```

## Event catalog

The catalog is a contract. Adding a kind extends this list in the same pull request.

| Group | Kinds |
|---|---|
| Session | `session.start`, `session.context` (BIOS context), `session.baseline` (baseline profile), `session.notice`, `session.archived` |
| Config and preflight | `config.loaded` (effective config), `preflight.check` (one per check, with result) |
| SMU | `smu.intent`, `smu.write`, `smu.readback`, `smu.error` |
| Profile | `profile.applied` (every application of every core's offset, with its condition), `profile.change` (the profile under guard: on entering guard, with `from` null, and after every guard decision) |
| Trials | `trial.intent`, `trial.start`, `trial.progress` (backend milestones such as a finished FFT size), `trial.signal` (load-step schedule), `trial.sample` (containment or stall warnings only), `trial.end` |
| Evidence | `failure` (kind, attribution, evidence `seq`), `mce` (raw and decoded lines, cpu, core, bank type), `crash.detected` (previous boot ID, in-flight action) |
| Tuner | `tuner.decision`, `core.phase`, `guard.rotation` (start and end), `escalation.window` (open and close), `tier.change` (from, to, reason) |
| Commands | `command.regain` (the cores queued), `command.reset` (a core, or all) |
| Stop | `deadend` (condition, evidence, action taken), `boot.saved_entry` (GRUB change), `shutdown` (clean stop) |
| Journal | `journal.torn`, `state.rebuilt` |

## State file

`state.json` is rewritten atomically after every event that changes it: temp file, fsync, rename, directory fsync. It is shaped for one-glance reading:

- `schema`, `session` (id, start time, BIOS context, baseline), `last_seq`;
- `phase`: `per_core` while any core is in search or confirmation, else `regain` while any core is in regain, else `guard`;
- `cores[]`: `core`, `ccd`, `cpus`, `offset`, `baseline`, `phase`, `pass`, `failed_mark`, `unproven_depth`, `queued` (`regain` or `reset` while a command waits for the next `run`, else omitted), and `last_decision` (its `seq` and `msg`);
- `in_flight`: the most recent intent without a result yet, or null;
- `dead_end`: the condition and its `seq`, or null. It clears when the next `run` starts (`config.loaded`);
- `guard`: null before the first `profile.change`, then `rotation` and `rotation_open`, the rotation's `steps` and `steps_done`, the `profile` and its `profile_seq`, `clean_rotations` and `clean_s` since that `profile.change`, `regimes[]` with each regime's `clean_s` and `rate_bound_per_h` (R1 to R7 in order), `escalation_window_open`, the overall `rate_bound_per_h`, and `tctl_max_c` with its `tctl_max_seq`, the highest Tctl among the passed resident trials since that `profile.change`. A rate bound is `3 / clean hours` per hour, rounded to 4 decimals, and null without clean hours;
- `tier` and `tier_seq`: the tier of the last `tier.change` and its `seq`; `none` and 0 before the first.

## Human-readable log

Each event's `msg` goes to stderr, prefixed by local time and kind padded to 14 characters, so the system journal of a tuning boot reads as a narrative:

```
01:14:07 trial.intent   trial 0413 core 07 CO -32 R2 mprime AVX2 36K-248K 90s isolated
01:15:37 trial.end      trial 0413 PASS 90s | Tctl max 71°C
01:15:37 tuner.decision core 07 passed R1+R2 at -32; next -37 (coarse, no failed mark yet)
```

`shycler events` renders the journal the same way:

```
shycler events [--core N] [--kind K[,K...]] [--trial ID] [--since RFC3339] [--until RFC3339] [--json]
```

- `--core N`: events whose `core` is N or whose `cores` include N.
- `--kind`: exact kinds, or a group when the entry has no `.` (`trial` matches every `trial.*` kind).
- `--trial ID`: events whose `trial` is ID.
- `--since` (inclusive) and `--until` (exclusive): a time window.
- `--json`: print the raw event lines instead.

All given filters must match. A torn tail is reported on stderr and left for the next `run` to record.
