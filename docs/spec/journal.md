# Journal and state

Normative rules for what togi records. Goal: anyone, human or agent, reading only the journal can reconstruct everything togi did and why, without reading code.

## Files

All files live in the state directory, default `/var/lib/togi`:

| Path | Role |
|---|---|
| `events.jsonl` | The journal of the current session: one JSON event per line, append-only, source of truth |
| `state.json` | The current state, a projection of the journal for readers |
| `trials/<trial-id>/` | Per-trial work directory: backend config files, raw stdout/stderr, backend result files |
| `trials/<trial-id>/samples.jsonl` | Fsynced per-second conditions: milliseconds since trial timing began (`elapsed_ms`), optional `tctl_c`, CCD temperatures by label (`tccd_c`), loaded-core frequencies in MHz by core ID (`core_mhz`), and package power in watts (`package_power_w`) |
| `archive/<session-id>.jsonl` | Journals of sessions ended by `reset --all` or by a transition |
| `archive/<session-id>-trials/` | The `trials/` directory of an archived session |
| `archive/<session-id>-compat-pending` | Crash-recovery marker while `reset --all` or a transition archives a journal with another schema; removed on completion |
| `archive/<session-id>-carry-pending` | Marker that the session a transition archived has not yet seeded a new session; removed once a journal records its `session.carried` or passes the point where one applies |
| `lock` | Held with `flock` by the one process allowed to write the journal |

Retention: trial directories of failed and inconclusive trials are kept forever. Passing ones are pruned beyond the newest 200.

## Rules

1. **Everything is an event.** These are all events:
   - every action with an effect outside togi's memory: SMU command, process start, signal or stop, scope creation, file written for a backend, sysctl, GRUB change;
   - every observation that feeds a decision;
   - every decision.

   Exceptions: `SIGSTOP`/`SIGCONT` cycles too frequent to record one by one. R3 and R4 cycles are recorded as the schedule and seed at trial start and as counts at trial end. R6 bursts are recorded as a `trial.progress` when they begin and as counts in a `trial.progress` at trial end. R7 runs separate trials on each loaded set of cores, not in-trial CCD phases. A refusal of a journal written by a newer schema or ruleset writes no event: the build that wrote this journal must remain able to resume it. The reason goes to stderr, which is the system journal in a tuning boot; clearing GRUB's saved entry for that refusal also writes no event. A transition archives an older journal without writing to it, for the same reason (Transitions, below).
2. **Intent before action.** An action that may crash the machine, such as an SMU write or a trial start, is appended and fsynced before it happens. On the next boot the last intent without a matching result is the in-flight action. Every event is written with a single `write` and fsynced before `Append` returns, so a crash loses at most the line being written (rule 6); closing the journal fsyncs it. Opening the journal fsyncs the state directory and its parent, so a newly created journal file survives a crash. `run` appends `shutdown` when it stops on a signal, at a dead end, or on reaching the requested rotations; other exits may record restoration but never `shutdown`, so a later boot whose journal ends without `shutdown` is treated as a crash (`workloads.md`, Failure signals). `tools/sim` fsyncs nothing, because a simulated crash cannot lose written data.
   A `deadend` without its `boot.saved_entry` (for a GRUB action) or without `shutdown` is an interrupted dead-end action, not permission to resume tuning: the next `run` completes the missing action and records `shutdown` before doing other work; a `run` without `--tuning-boot` cannot clear GRUB's saved entry and only records `shutdown`. A `boot.saved_entry` tied to that dead end is not cleared again. Once `shutdown` is present, the next `run` starts normally and re-evaluates dead-end conditions.
3. **Decisions name their cause.** A decision event carries `cause`: the `seq` numbers of the events it was derived from, plus a `reason` in plain words.
4. **The journal wins.** On start, togi rebuilds the state by replaying the journal. If `state.json` disagrees with the rebuild, it is rewritten and a `state.rebuilt` event records the difference.
5. **One writer.** `run` takes the host lock, then the state-directory `lock`, then prepares carry, and holds both locks through the session and its cleanup. The state-directory lock is held before any carry scan, archive or marker change, including a transition before its new journal exists. `reset` follows the same lock order and holds the state-directory lock through marker changes and archive completion; it refuses while a `run` holds it. The simulator takes the state-directory lock before carry and holds it through each simulated boot's session. `status`, `cert`, `events` and `watch` only read. `watch` projects the trial, progress, hunt masks, refinement checks and per-core replay, and logs `session.start`, `session.carried`, `host.ranking`, `trial.intent`, `trial.end`, `failure`, `mce`, `crash.detected`, `tuner.decision`, `tuner.warning`, `session.warning`, `core.phase`, `hunt.start`, `hunt.mask`, `hunt.end`, `hunt.skipped`, `mark.joint`, `refine.round`, `backend.retry`, `guard.rotation`, `profile.change`, `tier.change`, `deadend`, `defect.found`, `command.reset` and `shutdown`. `reset --core` ends with `shutdown` (reason `command`), so the next `run` never reads its boot as a crash. `reset --all` appends `command.reset` and `session.archived`, fsyncs, removes `state.json`, moves `trials/` to `archive/<session-id>-trials/`, then moves `events.jsonl` to `archive/<session-id>.jsonl`; it refuses before appending anything when either archive path already exists. Opening a journal whose last event is `session.archived` finishes that move first and continues with an empty journal. For an incompatible schema, `reset --all` cannot append: with the lock held it writes and fsyncs a temporary archive marker, removes the state file, moves `trials/` and then `events.jsonl` without writing to the old journal, fsyncing both directories after the renames, and removes the marker. A following `reset --all` resumes an interrupted move only if that marker exists; an archive path collision without the marker is refused. If the archive exists but `events.jsonl` is already gone, the next `reset --all` or journal open removes the marker and fsyncs the archive directory under the writer lock.
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
| `mono_ms` | Boot-local CLOCK_MONOTONIC milliseconds, on every event `run` appends; used to match evidence to trials despite wall-clock jumps. Absent in events `reset` appends, which are no trial evidence, and in events written by older builds |
| `msg` | Human-readable description, retaining raw diagnostic text as evidence; human output escapes controls |
| `cause` | Optional array of `seq` this event follows from |

Kind-specific fields are flat, snake_case and carry units in their names (`duration_s`, `period_ms`, `tctl_max_c`). Values use the vocabulary in `CONTEXT.md`. Cores are always `core` (the kernel `core_id`); logical CPUs are always `cpu`. The one exception to flat fields: `config.loaded` carries the effective configuration nested under `config`.

The `config.loaded.config` payload is a journal-owned snapshot, converted from effective configuration when the session records it. It preserves `start_offsets` and `candidate_edges` maps; `durations` with `search_trial_s`, `start_s`, `guard_trial_s`, `guard_idle_s`, `guard_all_core_s`; `evidence` with `miss` and `rate`; `guard.rotation`; `dead_ends.inconclusive_in_a_row` and `stray_crashes_in_a_row`; `backends.mprime` and `backends.ycruncher`; and `backend_user`. Null maps and rotations, empty maps and rotations, and zero values retain their representation. Changes to this persisted shape are journal schema decisions, independent of configuration implementation. Legacy `config.loaded` bodies remain decodable.

The first event is `session.start` with a flat build stamp: `version`, `rev`, `ruleset`, `schema` and `fixes`. `config.loaded` carries that stamp at every start or resume. This build uses ruleset 6 and journal schema 2. Missing ruleset means 1; absent fixes means 0. Added kinds and fields do not bump schema. Older journals are archived by transition, and newer journals are refused.

New session IDs are their start time in UTC to the second (`20060102T150405Z`). While holding the state-directory writer lock, a collision with either `archive/<id>.jsonl` or `archive/<id>-trials/` chooses the first free `-2`, `-3`, … suffix. Existing IDs are unchanged. Archive traversal orders equal-second suffixes numerically: the unsuffixed ID first, then `-2` through `-10` and beyond; different timestamps retain their order.

Build compatibility diagnostics consistently identify the program as togi, regardless of the recorded version.

One kind-to-payload constructor registry drives decoding and validates appends; an unregistered kind cannot be written. Within the same schema, `status`, `cert`, `events` and `watch` preserve unknown kinds as opaque events and skip facts they do not understand. `events` still displays their message and preserves their complete line with `--json`. `run` and both forms of `reset` refuse unknown kinds before modifying the journal, naming the kind and the journal and binary build stamps. This policy applies to builds from this change onward: 0.5.0 still rejects unknown kinds even for read-only commands.

Example trial, abbreviated:

```json
{"seq":812,"time":"2026-10-02T01:14:07.120000000Z","boot":"e8f9...","kind":"trial.intent","msg":"trial 0413 core 07 CO -32 R2 mprime AVX2 36K-248K 90s isolated","trial":"0413","core":7,"offset":-32,"regime":"R2","workload":"mprime-avx2-36k-248k","duration_s":90,"condition":"isolated","phase":"search","cause":[811]}
{"seq":815,"time":"2026-10-02T01:14:07.410000000Z","boot":"e8f9...","kind":"trial.start","msg":"trial 0413 started pid 48211 in scope togi-trial-0413 on cpu 7","trial":"0413","scope":"togi-trial-0413","pid":48211,"cpus":[7],"argv":["mprime","-t","-W/var/lib/togi/trials/0413"],"cause":[812]}
{"seq":819,"time":"2026-10-02T01:15:37.460000000Z","boot":"e8f9...","kind":"trial.end","msg":"trial 0413 PASS 90s | Tctl max 71°C","trial":"0413","outcome":"pass","duration_s":90,"tctl_max_c":71,"cause":[815]}
{"seq":820,"time":"2026-10-02T01:15:37.461000000Z","boot":"e8f9...","kind":"tuner.decision","msg":"core 07 passed R1+R2 at -32; next -37 (coarse, no failed mark yet)","core":7,"phase":"search","decision":"step_deeper","from_offset":-32,"to_offset":-37,"pass":-32,"failed_mark":null,"reason":"coarse, no failed mark yet","cause":[811,819]}
```

When a trial is closed on resume, its `trial.end` message names the last evidence rather than implying its `duration_s` measured the full run. After a crash, the last complete line of its `samples.jsonl` adds optional `last_sample_s` (elapsed milliseconds rounded down to seconds), `last_sample_tctl_c`, `last_sample_min_mhz` and `last_sample_max_mhz` (minimum and maximum of the available loaded-core frequencies). A torn last line is skipped. Missing files or sensors leave their fields absent. For example, a crashed trial says `trial 0889 FAIL crash, last evidence 0s after start, last sample 3s: Tctl 71°C, 5420-5610 MHz`; without samples it retains `trial 0330 FAIL crash, last evidence 0s after start`. An interrupted trial without a failure says `trial 0331 INCONCLUSIVE, last evidence 0s after start: togi stopped during the trial`. A trial ended while togi watches it, including an orderly stop by signal, keeps its existing message and measured duration.

## Event catalog

The catalog is a contract. Adding a kind extends this list in the same pull request.

| Group | Kinds |
|---|---|
| Session | `session.start` (session ID, cores and build stamp), `session.context` (BIOS context), `session.baseline` (baseline profile), `session.notice`, `session.archived`, `session.carried` (what a transition carries in, Transitions below) |
| Session warnings | `session.warning` (`operation`, optional `trial`, `error`): nonfatal session maintenance failure; passed-trial marker or prune failures cite the durable passing `trial.end`; state projection write failures cite the durable event when there is one |
| Config and preflight | `config.loaded` (effective config and build stamp), `preflight.check` (one per check, with result) |
| SMU | `smu.intent`, `smu.write`, `smu.readback`, `smu.error` |
| Profile | `profile.applied` (applied condition), `profile.change` (resident profile, `from` null on entering guard), `profile.restored` (before shutdown) |
| Trials | `trial.intent` (applied `profile` in core-id order, optional `hunt`, `mask`, `round`, `rerun`; message suffixes ` hunt H mask M`, ` round R`, ` rerun`), `trial.start` (pid, scope, cpus, argv, files, instances), `trial.progress` (optional backend `signal` and `core`), `trial.signal`, `trial.sample`, `trial.end` (optional `backend_missing`, `containment_error`; `core` on backend-ended resident and masked trials) |
| Evidence | `failure` (kind, attribution, evidence seq, optional applied `profile`), `mce`, `crash.detected` (previous boot, in-flight action, optional `reset_reason`, `reset_reason_raw`, `inconclusive`; message suffix `; reset reason: <raw or kind>` and `; inconclusive: <why>`) |
| Tuner | `tuner.decision` (`step_deeper`, `backoff`, `check_edge`, `deepen`, `yield`; `workloads` on edge check), `core.phase` (`check_edge`, `workloads`, `cleared_joint`), `guard.rotation` (start/end, end `qualifying` and `missing`), `tier.change` |
| Plans and marks | `host.ranking`, `hunt.start`, `hunt.mask`, `hunt.end`, `hunt.skipped`, `mark.joint`, `refine.round`, `tuner.warning` |
| Recovery | `backend.retry` |
| Defects | `defect.found` (known defect, direction, affected cores and decision sequences), `defect.answered` (reset answer and cores) |
| Commands | `command.reset` (a core, or all) |
| Stop | `deadend` (condition, evidence, action taken), `boot.saved_entry` (GRUB change), `shutdown` (clean stop) |
| Journal | `journal.torn`, `state.rebuilt` |

Backend computation errors, early exits and stalls are recorded as `trial.progress` with their typed `signal`, affected `core` and signal-specific human detail as soon as they are classified, before `trial.end`. Recovery retains this durable evidence if the machine crashes before the trial ends; a recorded backend failure outranks the reset reason. `trial.signal` records load-step signalling, not backend failures.

Kernel-log cursor persistence uses additive `kernel_cursor` and `kernel_error` fields on the already-existing `config.loaded`, `trial.intent`, `trial.end` and `shutdown` boundary events; it adds no boundary events or kinds. Replay keeps the newest cursor separately for each event's `boot`. A failed read retains the prior cursor, and its diagnostic stays with the intersecting trial even if teardown or same-boot resume later succeeds. An unresumable cursor records the lost interval and may persist a fresh anchor, without clearing that trial's error. A torn or interrupted boundary resumes the previous durable cursor and deduplicates already-recorded MCEs.

`trial.start.window_start_ns` records the boot-local monotonic instant after profile readback, before workload startup, so the evidence window does not depend on wall time or backend setup duration. If startup is interrupted before `trial.start`, readiness is reconstructed from the latest target readback's `mono_ms`, or the intent timestamp when the resident profile needed no target write.

Each `mce` records `monotonic_ns`, its boot-local kernel-log timestamp in nanoseconds, including zero. Its clock belongs to `from_boot` when present, otherwise the event's `boot`; it is not the time recovery appended the event. The field is additive under ADR 0009, with no schema or ruleset change. Events without it, including those written by 0.5.0, retain their previous attribution behavior.

An MCE whose timestamp is inside the inclusive profile-applied through teardown window names its `trial` on a normal boundary read; an outside-window MCE has `between_trials: true`, no trial, and the message suffix `between trials (recorded only)`. These events are visible in `events`, in `status`'s between-trial evidence and in `watch`'s recent log, but supply no trial outcome or failure decision. Crash-recovery MCEs retain `from_boot`; replay checks previous-boot timestamps against that boot's open trial, while uncorrected MCA evidence retained into the next boot is associated with the preceding crash without comparing the two boot clocks.
`trial.end.containment_error` records unconfirmed trial cleanup, including partial-start rollback. Such a trial is inconclusive as stability evidence and leads directly to the existing `containment` dead end, even if the trial was interrupted or produced a computation error. Replay retains this evidence if the process stops before recording the dead end; no next trial or tuning profile write follows.

`trial.intent.cores` lists exactly the loaded cores for R6 and R7. `profile` always lists applied offsets for every core in core-id order. `tuner.decision` with `decision: check_edge` has the frozen two `workloads`; its message says `checks its edge`. `deepen` says `deepened`, `yield` says `yielded`, search `backoff` says `failed`, and guard, hunt and refine `backoff` say `backed off`. `core.phase` uses `search`, `resident` or `done`, while intent and decisions may use activities `guard`, `hunt` and `refine`. An equal `profile.change` message does not say guard restarts.

Plan kinds and their payloads (optional fields are omitted when empty):

| Kind | Fields | `msg` shape |
|---|---|---|
| `host.ranking` | `ranking` (preferred cores), `values` (raw per core), `detail` (fallback) | `preferred cores 03 11 …` or `preferred-core ranking unavailable (<detail>); core-id order` |
| `hunt.start` | `hunt`, `failure` seq, optional `trial`, `regime`, `workload`, loaded `cores`, `duration_s`, `failing`, `anchor`, `anchor_seq` (0 for all-zero), `candidates`, `starts`, `start_s`, `miss`, `rate`, `ranking` | `hunt 3: unattributed crash in resident R7 trial 0412; anchor from rotation end #811; candidates 00 01 …; masks of 5 × 120s` |
| `hunt.mask` | `hunt`, `mask`, `cores`, `profile`, `set`, `granularity`, `stage` (`part`, `complement`, `full`, `edge`), `index`, `duration_s`, optional `escalated`, `full_checked`, `any_failed`, `edge` (probed `core` and `offset`) and `held` (the other members' `core` and `offset`, also listed in `cores`), `inferred` (`pass` or `failure`), `skipped`, `reason` | `hunt 3 mask 4: cores 08-11 at failing offsets, the rest at the anchor; 5 × 120s R7 starts` or `hunt 3 mask 9: core 11 at -22 with core 03 -40, the rest at the anchor; …` |
| `hunt.end` | `hunt`, `result` (`culprit`, `joint`, `fallback`, `direct`, `cancelled`), optional `cores`, `members` (each member's shallowest failing `core` and `offset`, on a probed `joint`), `masks`, `reason` | `hunt 3 found core 13 after 4 masks` |
| `hunt.skipped` | `failure` seq, `reason` | `failure #904 is not hunted: <reason>` |
| `mark.joint` | `mark`, `members` (`core`, `offset` pairs), optional `fallback`, `hunt`, `reason` | `joint mark J2: core 03 -40 + core 11 -30, observed in hunt 4` (or `…, fallback over every candidate of hunt 4`) |
| `refine.round` | `round`, `event` (`start`, `end`); start: `anchor`, `anchor_seq`, `target`, proposed `profile`, changed `cores`, `ranking`, `starts`, `start_s`; end: optional `passed`, `reason` | `refine round 2 start: 5 cores toward …` / `refine round 2 end: passed` |
| `tuner.warning` | `warning` (`monotonicity`), `trial`, `passes` (contradicted trial-end seqs), `detail` | `monotonicity: trial 0520 failed on a profile at least as shallow as 5 passes in its class` |
| `backend.retry` | `backend` (or `kernel_log`), `attempt` (1–3), `wait_s` (60, 300, 1800), `reason` | `backend mprime: retry 2 of 3 after 300s: setup failed: …` |

## Transitions

A transition archives an older ruleset or schema journal before opening it; it also archives a compatible journal when the recorded BIOS context differs from the current one. The same carry marker, lock and crash-recovery protocol apply. A BIOS change carries candidate edges only, not failed or joint marks. A newer journal remains a refusal. Before the move, `run` writes and fsyncs `archive/<session-id>-carry-pending` without appending to the old journal. A previously archived last `session.archived` move is completed without carry. `reset --all` removes carry markers. A malformed complete archived line without `kind` stops carry with an error.

While a carry marker exists and the current journal holds neither a `session.carried` whose first source is the marked session nor any `core.phase`, the next `run` computes the carry from the archives and records it once, after `session.context`, the baseline and its notice, and before the first `core.phase`. Afterwards the marker is removed. A crash between `session.carried` and the phases resumes from the recorded event, never from the archives.

If a further transition archives an interrupted new session that recorded neither `session.context` nor `session.carried`, the earlier pending source is retained instead of being replaced by that incomplete session. Its edges, marks, BIOS context and original provenance seed the eventual session. Once context or carry is recorded, normal source chaining and per-core reset epochs apply. An explicit `reset --all` still drops every pending source.

Archives use a lenient reader accepting every shipped schema. It decodes `session.start`, `session.context`, `session.carried`, `trial.intent`, `trial.end`, `trial.progress`, `failure`, `hunt.start`, `hunt.end`, `command.reset`, `shutdown`, `tuner.decision`, `defect.found` and the `config.loaded` build stamp, skipping unrelated kinds and torn tails.

Sources, newest first: the archived session, then, when it holds no `session.carried` and recorded a BIOS context, each older archive in turn, for as long as the archive recorded the same BIOS context and a ruleset different from the source read before it. A walked archive that holds a `session.carried` is the last source. Per source, a value recorded before a `reset --core` of its core in that source is dropped:
- **Candidate edge:** the deepest offset of a passing `trial.end` whose `trial.intent` is isolated with one core and an offset.
- **Carried mark:** the shallowest offset of an attributed `failure` of one core at a known offset (including 0), or a `hunt.end` `culprit` for core c at the `hunt.start` failing profile's offset c, with the cited `failure` signal and hunt-end sequence. Both follow the same reset epoch and defect exclusions; direct hunt failures already appear as attributed `failure` events. Joint marks are not carried. An isolated nonzero trial left in flight after the last shutdown also counts as a crash failure.
- **Carried values:** the edges and marks of a `session.carried` in the source, dropped by a later `reset --core` of their core in that source.

Across sources the deepest edge and the shallowest mark win; on a tie, the first found. Passes, resident offsets and unattributed failures are never carried.

`session.carried` has no cause and carries `sources` (each with `session`, `path`, `schema` and `ruleset`), `marks`, an optional `detail`, and `carried`: one entry per core of this machine that has an edge or a mark, with `core`, and `edge`, `edge_session` and `edge_seq` (the passing `trial.end`), and `failed_mark`, `mark_session`, `mark_seq` and `mark_signal` where present. `marks` is false, and `detail` says why, when the archived session recorded no BIOS context or a different one from this machine's; the marks then stay behind and only edges are carried. Its `msg` is `carried N candidate edges and M failed marks from session <id> (schema S, ruleset R), …`, or, without marks, `carried N candidate edges from …; failed marks stay behind: <detail>`. `status` prints it as `carried: [#seq] <msg>`. How the new session starts from these values is in `tuner.md`, Session start.

## Defects

The binary's defect list gives each entry an increasing integer `id`, short `title`, fixing pull request `pr`, affected decision `kind` and `decision`, required cause kind, optional predicate on surrounding events, and `direction` (`too_cautious` or `too_aggressive`). The highest ID is the binary's `fixes` stamp. A decision uses the most recent `session.start` or `config.loaded` `fixes` before it (absent means 0). A matching decision recorded with fixes below that entry's ID is not recomputed or removed: on resume, the build appends one `defect.found` per defect per session, aggregating the affected `cores` and `decisions` (their `seq`s); `cause` lists those decision sequences. A prior `defect.found` suppresses another finding for that ID.

The first entry, ID 1, is the false failure on power-off fixed in [#14](https://github.com/shgew/togi/issues/14), direction `too_cautious`. It matches a `tuner.decision` with `decision: backoff` citing a `failure` with `signal: unexpected_exit`, whose trial's `trial.end` failed with the same signal, whose `trial.progress` in that boot before the end has `detail: "core NN backend exited early: <nil>"` for the affected core, and whose boot has a signal `shutdown` after the decision within 5 seconds of that `trial.end`. The `<nil>` status recorded the clean backend exit in pre-fix builds; an `exit status N` or `signal: ...` indicates a real failure even if a signal shutdown follows immediately. The observed failure preceded the shutdown by about 100 ms; 5 seconds allows clean-stop work between them. An unrelated unexpected backend exit without that same-boot shutdown is not a match.

`defect.found` carries `id`, `title`, `detail` (why), `pr`, `direction`, `cores` and `decisions`. `defect.answered` carries `id`, `cores` and `answer` (`yes` or `no`), with `cause` citing the finding. The answer is appended and fsynced first. A yes then appends one `command.reset` per affected core, each citing the durable answer, not the finding; a no records only the answer. On resume, a recorded yes queues only cores still lacking a reset caused by that answer, before queued resets or tuning decisions are consumed. Manual resets and resets caused by another finding or answer do not satisfy that missing work. Either answer prevents another prompt for that ID; an answer does not hide a too-cautious finding from `status`. `status` keeps showing the finding's reset command for each affected core until a later `command.reset` for that core or `reset --all`. The original decisions and every journal line remain unchanged.

Older builds recorded all finding-caused resets immediately before the yes answer. A complete preceding block with the answer's cores and finding cause is already finished; resume preserves any progress learned afterward. Earlier consumed resets outside that block do not satisfy a new answer.

## State file

`state.json` is rewritten atomically after every event that changes it: temp file, fsync, rename, directory fsync.

A failed projection write appends `session.warning` with `operation: "write state projection"` and the error, then continues the session; it never triggers emergency zeroing by itself. The authoritative event is already durable. Persisting that warning does not attempt another projection write, so an unwritable state file cannot recursively generate warnings. A journal append failure, including failure to append the warning, remains fatal: after CPU family/model and driver codename validation it takes the emergency-zeroing path; before that boundary it exits without mailbox commands or SMN access (`runtime.md`, Preflight). Readers see either the previous complete state or the new complete state, never a torn file. On the next start, replay rebuilds a stale or missing projection and records `state.rebuilt` after a successful rewrite; if that rewrite fails, it warns and continues instead. A warning after a durable `shutdown` does not make the stopped boot a crash.

It is shaped for one-glance reading:

- `schema` (2), `session` (id, start time, BIOS context, baseline), `last_seq`;
- `phase`: activity `search`, `hunt`, `refine` or `guard`;
- `cores[]`: core identity, offset, baseline, phase (`search`, `resident`, `done`), pass, failed mark, `joint_marks` IDs, queued reset and last decision;
- `in_flight`: most recent intent without a result, or null;
- `dead_end`: condition and sequence, or null (cleared on the next `config.loaded`);
- `joint_marks[]`: `mark`, `members` (core/offset pairs), `fallback`, `hunt`, `seq`;
- `hunt`: null or `hunt`, `seq`, `failure`, `regime`, `trial`, `anchor`, `anchor_seq`, `candidates`, `escalated`, `masks[]` (`mask`, `seq`, `cores`, optional `edge` and `held`, `outcome` running/pass/failure/skipped, `passes`, `needed`);
- `refine`: null or `round`, `seq`, `target`, `profile`, `cores`, `checks[]` (`regime`, `workload`, `cores`, `passes`, `needed`);
- `guard`: null before the first `profile.change`; then rotation progress, profile, `qualifying`, `missing`, `tier_clock_seq`, `clean_rotations`, `clean_s`, regime exposure and bound, `exposure[]` (`regime`, `workload`, `starts`, `clean_s`, `rate_bound_per_h`), and highest counted Tctl. Bounds are `3 / clean hours`, rounded up to 4 decimals, null without hours;
- `tier` and `tier_seq`, or `none` and 0 before the first change.

## Human-readable log

Each event's `msg` goes to stderr in an escaped representation, prefixed by local time and kind padded to 14 characters, so the system journal of a tuning boot reads as a narrative:

```
01:14:07 trial.intent   trial 0413 core 07 CO -32 R2 mprime AVX2 36K-248K 90s isolated
01:15:37 trial.end      trial 0413 PASS 90s | Tctl max 71°C
01:15:37 tuner.decision core 07 passed R1+R2 at -32; next -37 (coarse, no failed mark yet)
```

Human rendering escapes untrusted controls before adding application-owned styling. C0 controls and DEL use visible `\xNN` escapes, except tab, newline and carriage return use `\t`, `\n` and `\r`. C1 controls (including U+009B CSI and U+009D OSC), Unicode line and paragraph separators, and bidi formatting controls and marks (U+061C, U+200E–U+200F, U+202A–U+202E and U+2066–U+2069) use `\uNNNN`; raw ESC is `\x1b`. Invalid UTF-8 bytes use `\xNN`. Other Unicode and already-visible escape text remain readable and unchanged. The same rule covers event kinds, including unknown kinds in read errors, journal-derived status and certificate fields (including BIOS context), archive notices, and dashboard messages and diagnostics, so one event cannot insert a terminal command, another rendered line or a bidi override. Application-owned ANSI styling and dashboard cursor controls remain active.

The journal's `msg`, other evidence fields, and raw `togi events --json` lines are never sanitized or restyled: the escaped text exists only at human rendering boundaries.

`togi events` renders the journal the same way:

```
togi events [--core <N>] [--kind <kinds>] [--trial <ID>] [--since <time>] [--until <time>] [--json]
```

- `--core N`: events whose `core` is N, whose `cores` or `candidates` include N, or whose `members[].core` is N.
- `--kind`: comma-separated known exact kinds, or a known group when the entry has no `.` (`trial` matches every `trial.*` kind). Names come from the binary's kind registry. An unknown name or explicitly empty list exits 2 with the error and valid names on stderr before reading the journal; a valid filter with no matches exits 0 with empty stdout.
- `--trial ID`: events whose `trial` is ID.
- `--since` (inclusive) and `--until` (exclusive): a time window.
- `--json`: print the raw event lines instead.

All given filters must match. A torn tail is reported on stderr and left for the next `run` to record.

### Colors

The run log and `togi events` color the whole human-readable line according to its moment:

| Moment | Color |
|---|---|
| Trial ends with failure (`trial.end` outcome `failure`), `failure`, `crash.detected` | Red |
| Dead end (`deadend` event and `run` summary), incompatible-session refusal line (not an event) | Red, bold |
| Search step passed (`step_deeper`), refinement `deepen`, or `hunt.end` result `culprit`, `joint` or `direct` | Green |
| Core becomes done, search becomes resident, passed `refine.round` end, clean qualifying `guard.rotation` end, or tier earned | Green, bold |
| `mark.joint`, `hunt.start`, `tuner.warning`, `session.warning` (including state projection write failures), `backoff` or `yield`; known defect or ruleset mismatch warning | Yellow |
| Inconclusive trial or `backend.retry` | Dim |
| `hunt.mask`, `refine.round` start, `hunt.skipped`, `host.ranking`, clean non-qualifying rotation, and everything else | Plain |

ANSI SGR is used when that output stream is a terminal (character device) or `JOURNAL_STREAM` names that stream (its device and inode match). A non-empty `NO_COLOR` disables ANSI even in the system journal. `events.jsonl`, `state.json` and `togi events --json` stay uncolored. When `JOURNAL_STREAM` names that stream, red and red-bold lines start with `<3>` so systemd stores them at priority err (`SyslogLevelPrefix=` is on by default), including with `NO_COLOR`; no other line gets a priority prefix, and terminals outside the system journal never get one. `togi events` uses the same rules on stdout.

Every new event kind names its row here, or plain.

