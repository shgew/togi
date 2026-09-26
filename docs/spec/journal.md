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
| `archive/<session-id>-trials/` | The `trials/` directory of a session ended by `reset --all` |
| `archive/<session-id>-compat-pending` | Crash-recovery marker while `reset --all` archives a journal with another schema; removed on completion |
| `lock` | Held with `flock` by the one process allowed to write the journal |

Retention: trial directories of failed and inconclusive trials are kept forever. Passing ones are pruned beyond the newest 200.

## Rules

1. **Everything is an event.** These are all events:
   - every action with an effect outside shycler's memory: SMU command, process start, signal or stop, scope creation, file written for a backend, sysctl, GRUB change;
   - every observation that feeds a decision;
   - every decision.

   Exceptions: `SIGSTOP`/`SIGCONT` cycles too frequent to record one by one. R3 and R4 cycles are recorded as the schedule and seed at trial start and as counts at trial end. R6 bursts are recorded as a `trial.progress` when they begin and as counts in a `trial.progress` at trial end. R7 runs separate trials on each loaded set of cores, not in-trial CCD phases. A refusal caused by a schema or ruleset mismatch writes no event: the build that wrote this journal must remain able to resume it. The reason goes to stderr, which is the system journal in a tuning boot; clearing GRUB's saved entry for that refusal also writes no event.
2. **Intent before action.** An action that may crash the machine, such as an SMU write or a trial start, is appended and fsynced before it happens. On the next boot the last intent without a matching result is the in-flight action. Every event is written with a single `write` and fsynced before `Append` returns, so a crash loses at most the line being written (rule 6); closing the journal fsyncs it. Opening the journal fsyncs the state directory and its parent, so a newly created journal file survives a crash. `run` appends `shutdown` when it stops on a signal, at a dead end, or on reaching the requested rotations; any other exit writes nothing, so a later boot whose journal ends without `shutdown` is treated as a crash (`workloads.md`, Failure signals). `tools/sim` fsyncs nothing, because a simulated crash cannot lose written data.
   A `deadend` without its `boot.saved_entry` (for a GRUB action) or without `shutdown` is an interrupted dead-end action, not permission to resume tuning: the next `run` completes the missing action and records `shutdown` before doing other work; a `run` without `--tuning-boot` cannot clear GRUB's saved entry and only records `shutdown`. A `boot.saved_entry` tied to that dead end is not cleared again. Once `shutdown` is present, the next `run` starts normally and re-evaluates dead-end conditions.
3. **Decisions name their cause.** A decision event carries `cause`: the `seq` numbers of the events it was derived from, plus a `reason` in plain words.
4. **The journal wins.** On start, shycler rebuilds the state by replaying the journal. If `state.json` disagrees with the rebuild, it is rewritten and a `state.rebuilt` event records the difference.
5. **One writer.** `run` and `reset` take `lock` before appending; `reset` refuses while a `run` holds it. `status`, `cert`, `events` and `watch` only read. `watch` projects `session.start`, `trial.intent`, `trial.start` and `trial.end` (the in-flight trial, its progress and the last Tctl), `failure`, `crash.detected` and `deadend`, and logs `session.start`, `trial.intent`, `trial.end`, `failure`, `crash.detected`, `tuner.decision`, `core.phase`, `guard.rotation`, `profile.change`, `tier.change`, `deadend`, `defect.found`, `command.reset` and `shutdown`; the per-core state comes from the replay. `reset --core` ends with `shutdown` (reason `command`), so the next `run` never reads its boot as a crash. `reset --all` appends `command.reset` and `session.archived`, fsyncs, removes `state.json`, moves `trials/` to `archive/<session-id>-trials/`, then moves `events.jsonl` to `archive/<session-id>.jsonl`; it refuses before appending anything when either archive path already exists. Opening a journal whose last event is `session.archived` finishes that move first and continues with an empty journal. For an incompatible schema, `reset --all` cannot append: with the lock held it writes and fsyncs a temporary archive marker, removes the state file, moves `trials/` and then `events.jsonl` without writing to the old journal, fsyncing both directories after the renames, and removes the marker. A following `reset --all` resumes an interrupted move only if that marker exists; an archive path collision without the marker is refused. If the archive exists but `events.jsonl` is already gone, the next `reset --all` or journal open removes the marker and fsyncs the archive directory under the writer lock.
   If the old schema already recorded `session.archived` before the update, `reset --all` completes that recorded move without creating a marker, including when `trials/` was moved before the interruption.
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

The first event of a session is `session.start` with a flat build stamp: `version`, `rev`, `ruleset`, `schema` and `fixes` (the last three are integers). `config.loaded` carries the same fields at every start or resume. The starting stamp determines the session's ruleset and schema; the last stamped `config.loaded` identifies the build and fixes used most recently. This build uses ruleset 2 and schema 2. Missing ruleset means ruleset 1; an absent version means the writing build is unknown, and absent fixes means 0. Only bump `journal.Schema` when this build cannot read an older journal the same way: adding fields or kinds does not bump it. Schema-1/ruleset-1 sessions are refused by this build and can be archived with `reset --all`.

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
| Session | `session.start` (session ID, cores and build stamp), `session.context` (BIOS context), `session.baseline` (baseline profile), `session.notice`, `session.archived` |
| Config and preflight | `config.loaded` (effective config and build stamp), `preflight.check` (one per check, with result) |
| SMU | `smu.intent`, `smu.write`, `smu.readback`, `smu.error` |
| Profile | `profile.applied` (every application of every core's offset, with its condition), `profile.change` (the profile under guard: on entering guard, with `from` null, and after every guard decision), `profile.restored` (the offsets written back before `shutdown`, `runtime.md`) |
| Trials | `trial.intent`, `trial.start` (pid, scope, cpus and argv; the config `files` written; `instances` with core, cpus, pid and scope when the trial runs more than one), `trial.progress` (backend milestones such as a finished FFT size), `trial.signal` (load-step schedule), `trial.sample` (containment or stall warnings only), `trial.end` |
| Evidence | `failure` (kind, attribution, evidence `seq`), `mce` (raw and decoded lines, cpu, core, bank type), `crash.detected` (previous boot ID, in-flight action) |
| Tuner | `tuner.decision` (including `decision: regain` at a clean rotation end), `core.phase`, `guard.rotation` (start and end), `tier.change` (from, to, reason) |
| Defects | `defect.found` (known defect, direction, affected cores and decision sequences), `defect.answered` (reset answer and cores) |
| Commands | `command.reset` (a core, or all) |
| Stop | `deadend` (condition, evidence, action taken), `boot.saved_entry` (GRUB change), `shutdown` (clean stop) |
| Journal | `journal.torn`, `state.rebuilt` |

`trial.intent.cores` lists exactly the loaded cores for R6 and R7: R6 lists all cores; a single-CCD R7 trial lists that CCD's cores, and its final trial lists all cores. `tuner.decision` with `decision: regain` has phase `guard`, cites the clean `guard.rotation` end, moves from `from_offset` to `to_offset` one count deeper, and records the resulting `unproven_depth`. Guard decisions also record optional `settled_mark` (the shallowest settled offset) and `spent_steps` (sorted numeric offsets whose single retry has been used). An absent settled mark means none. The decision spends the retry before subsequent SMU writes, so replay follows the recorded state without re-deciding it. `core.phase` does not use a `regain` phase or a `backoff` field.

## Defects

The binary's defect list gives each entry an increasing integer `id`, short `title`, fixing pull request `pr`, affected decision `kind` and `decision`, required cause kind, optional predicate on surrounding events, and `direction` (`too_cautious` or `too_aggressive`). The highest ID is the binary's `fixes` stamp. A decision uses the most recent `session.start` or `config.loaded` `fixes` before it (absent means 0). A matching decision recorded with fixes below that entry's ID is not recomputed or removed: on resume, the build appends one `defect.found` per defect per session, aggregating the affected `cores` and `decisions` (their `seq`s); `cause` lists those decision sequences. A prior `defect.found` suppresses another finding for that ID.

The first entry, ID 1, is the false failure on power-off fixed in pull request #16, direction `too_cautious`. It matches a `tuner.decision` with `decision: backoff` citing a `failure` with `signal: unexpected_exit`, whose trial's `trial.end` failed with the same signal, whose `trial.progress` in that boot before the end has `detail: "core NN backend exited early: <nil>"` for the affected core, and whose boot has a signal `shutdown` after the decision within 5 seconds of that `trial.end`. The `<nil>` status recorded the clean backend exit in pre-fix builds; an `exit status N` or `signal: ...` indicates a real failure even if a signal shutdown follows immediately. The observed failure preceded the shutdown by about 100 ms; 5 seconds allows clean-stop work between them. An unrelated unexpected backend exit without that same-boot shutdown is not a match.

`defect.found` carries `id`, `title`, `detail` (why), `pr`, `direction`, `cores` and `decisions`. `defect.answered` carries `id`, `cores` and `answer` (`yes` or `no`), with `cause` citing the finding. A yes first appends one `command.reset` per affected core, each citing the finding, and then records the answer; a no records only the answer. Either answer prevents another prompt for that ID; an answer does not hide a too-cautious finding from `status`. `status` keeps showing the finding's reset command for each affected core until a later `command.reset` for that core or `reset --all`. The original decisions and every journal line remain unchanged.

## State file

`state.json` is rewritten atomically after every event that changes it: temp file, fsync, rename, directory fsync. It is shaped for one-glance reading:

- `schema` (2), `session` (id, start time, BIOS context, baseline), `last_seq`;
- `phase`: `per_core` while any core is in search or confirmation, else `guard`;
- `cores[]`: `core`, `ccd`, `cpus`, `offset`, `baseline`, `phase`, `pass`, `failed_mark`, `unproven_depth`, `settled_depth`, `queued` (`reset` while a command waits for the next `run`, else omitted), and `last_decision` (its `seq` and `msg`). `unproven_depth` includes settled depth; regainable depth is `unproven_depth - settled_depth`;
- `in_flight`: the most recent intent without a result yet, or null;
- `dead_end`: the condition and its `seq`, or null. It clears when the next `run` starts (`config.loaded`);
- `guard`: null before the first `profile.change`, then `rotation` and `rotation_open`, the rotation's `steps` and `steps_done`, the `profile` and its `profile_seq`, `clean_rotations` and `clean_s` since that `profile.change`, `regimes[]` with each regime's `clean_s` and `rate_bound_per_h` (R1 to R7 in order), the overall `rate_bound_per_h`, and `tctl_max_c` with its `tctl_max_seq`, the highest Tctl among the passed resident trials since that `profile.change`. A rate bound is `3 / clean hours` per hour, rounded up to 4 decimals so it never understates the bound, and null without clean hours;
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
shycler events [--core <N>] [--kind <kinds>] [--trial <ID>] [--since <time>] [--until <time>] [--json]
```

- `--core N`: events whose `core` is N or whose `cores` include N.
- `--kind`: exact kinds, or a group when the entry has no `.` (`trial` matches every `trial.*` kind).
- `--trial ID`: events whose `trial` is ID.
- `--since` (inclusive) and `--until` (exclusive): a time window.
- `--json`: print the raw event lines instead.

All given filters must match. A torn tail is reported on stderr and left for the next `run` to record.

### Colors

The run log and `shycler events` color the whole human-readable line according to its moment:

| Moment | Color |
|---|---|
| Trial ends with failure (`trial.end` outcome `failure`), `failure`, `crash.detected` | Red |
| Dead end (`deadend` event and `run` summary), incompatible-session refusal line (not an event) | Red, bold |
| Search step passed (`tuner.decision` with `decision: step_deeper`), automatic regain (`tuner.decision` with `decision: regain`) | Green |
| Core confirmed, edge reported (`core.phase` from `confirmation` to `confirmed`, or `search` to `confirmation` for a candidate edge) | Green, bold |
| Clean guard rotation (`guard.rotation` end with `clean: true`), tier earned (`tier.change` to a higher tier) | Green, bold |
| Proven or suspect backoff (`tuner.decision` with `decision: backoff` or `suspect_backoff`) | Yellow |
| A known defect found (`defect.found`) | Yellow |
| Ruleset mismatch warning on read-only commands (not an event) | Yellow |
| Inconclusive trial (`trial.end` outcome `inconclusive`) | Dim |
| Everything else, including `defect.answered` and a single trial passing | Plain |

ANSI SGR is used when that output stream is a terminal (character device) or `JOURNAL_STREAM` names that stream (its device and inode match). A non-empty `NO_COLOR` disables ANSI even in the system journal. `events.jsonl`, `state.json` and `shycler events --json` stay uncolored. When `JOURNAL_STREAM` names that stream, red and red-bold lines start with `<3>` so systemd stores them at priority err (`SyslogLevelPrefix=` is on by default), including with `NO_COLOR`; no other line gets a priority prefix, and terminals outside the system journal never get one. `shycler events` uses the same rules on stdout.

Every new event kind names its row here, or plain.

