# Tuner

Normative rules for how shycler moves offsets. Terms are defined in `CONTEXT.md`. Regimes, workloads and failure detection are in `workloads.md`; how decisions are recorded is in `journal.md`.

## Invariants

1. Every offset stays within [-50, 0]. shycler never writes a positive offset.
2. During an isolated trial only the target carries a nonzero offset. Every other core is written to 0 first, including at the start of each boot, when firmware has restored the BIOS values.
3. Every SMU write is preceded by a durable intent event and followed by a readback. A readback that differs from the written value is a dead end.
4. A core never runs at or deeper than its failed mark, except after `reset`.
5. After confirmation, offsets only get shallower, except through an explicit `regain`.
6. Every decision is an event that names its cause (`journal.md`).

## Session start

1. Preflight (`runtime.md`) passes, or shycler stops at a dead end.
2. The first `run` of a session reads every core's offset from the SMU as the baseline and records the BIOS context.
3. Each core's start offset is the configured override if one exists, else its baseline, clamped to [-50, 0].
4. A nonzero baseline produces a notice recommending BIOS CO 0 for tuning. shycler still starts from it.
5. A later `run` whose BIOS context differs from the session's is a dead end until `reset --all` starts a new session.

## Scheduling

Cores are visited in CCD-alternating order: 0, 8, 1, 9, ... 7, 15. Each slot goes to the next core in that order that is still in search or confirmation, and that core runs its next step. Interleaving cores this way gives every core time to cool between its own trials.

A slot is one search step (R1, then R2 if R1 passed) or one confirmation trial. An inconclusive trial is retried at once in the same slot.

## Search

A search step is two isolated trials at the same offset: R1, then R2. The step passes when both pass. A step whose R1 fails ends without R2.

Each core tracks its current offset `o`, `pass` (the deepest offset with a passed step, or none) and `fail` (its failed mark, or none).

After a passed step at `o`:
- `pass = o`.
- `o == -50`: `-50` is the candidate edge.
- `fail` is none: next offset is `max(o - 5, -50)`.
- Otherwise the next offset is `o - 1`. If that equals `fail`, `o` is the candidate edge.

After an attributed failure at `o`:
- `fail = max(fail, o)`, keeping the shallowest failure.
- If `pass` is at or deeper than `fail`, it is discarded, because the new failure contradicts it.
- `o == 0`: dead end.
- `pass` is none: next offset is `min(o + 5, 0)`.
- Otherwise the next offset is `pass - 1`, or `pass` is the candidate edge if `pass - 1 == fail`.

Any failure during an isolated trial is attributed to the target, crashes included: no other core carries an offset that could explain it.

## Confirmation

At the candidate edge `e`, the core runs one isolated trial per regime R1 to R5.
- All pass: the core is confirmed and `e` is its edge.
- An attributed failure sets `fail = e` and moves the core to `e + 1`, where confirmation restarts from R1. A failure at `e == 0` is a dead end.

## Isolated trial sequence

Between trials every core is at 0. Every `run` start in per-core phases writes every core to 0 with one set-all command and reads each back; `profile.applied` is recorded the first time in each boot. One isolated trial is then:

1. `trial.intent`;
2. SMU set target to its offset (skipped at 0);
3. `trial.start`, plus `trial.signal` with the load-step schedule for R3 and R4;
4. the trial, then `trial.signal` with the SIGSTOP and SIGCONT counts for R3 and R4;
5. SMU set target to 0 (skipped at 0);
6. `mce` events for the trial window;
7. `trial.end`;
8. the tuner's `failure` when the trial failed.

## Crashes

In per-core phases a crash is classified by what its boot recorded:
- A trial in flight in the crashed boot: a failure of that trial, attributed to its target at its offset.
- `profile.applied` in that boot and no trial in flight: an unattributed failure that changes nothing, since every core was at 0.
- No `profile.applied` in that boot: a stray crash. Stray crashes count in a row until the next `profile.applied`; reaching `dead_ends.stray_crashes_in_a_row` is the boot-loop dead end.

## Decision events

Moves within a phase are `tuner.decision` events: `step_deeper` after a passed search step, `backoff` after an attributed failure. Reaching a candidate edge (search to confirmation) and passing confirmation (confirmation to confirmed) are `core.phase` events. Both carry the resulting `pass` and `failed_mark`, so replaying the journal never re-runs a rule.

## Guard

Guard starts once every core is confirmed. The profile of all edges is applied and stays applied between trials.

A rotation runs the schedule in `workloads.md`. It restarts from the first step whenever the profile changes, so a clean rotation always covers one unchanged profile.

Failure handling:
- **Attributed** failure on core `c` at offset `o`: proven backoff, so `fail = o` and `c` moves to `o + 1`. Isolated confirmation at `o` already covers the shallower `o + 1`, so no re-confirmation runs. `o == 0` is a dead end.
- **Unattributed** failure:
  - The escalation window is closed and the trial had a single target core `c`: suspect backoff of `c` by one count, and the window opens.
  - Otherwise, meaning the window was already open or the trial was R6 or R7: suspect backoff by one count on every core with a nonzero offset, and the window stays open.
  - The window closes when a rotation completes clean.
- A crash with the profile applied and no trial in flight counts as an unattributed failure of an R6 trial.
- **Inconclusive** trials are retried and change nothing.

Every backoff changes the profile: clean hours reset and the rotation restarts.

## Regain

Suspect backoffs are never undone automatically. `shycler status` lists each core's unproven depth. `shycler regain [--core N]` takes each selected core with unproven depth, in scheduling order, one count deeper per invocation:

1. Move the core one count deeper, undoing one suspect step.
2. Run isolated confirmation R1 to R5 at that offset.
3. Pass: that count is regained, and its unproven depth drops by one.
4. Attributed failure: proven backoff. The failed mark cancels the core's remaining unproven depth, since those steps were all deeper.
5. Guard resumes with the new profile, and clean hours reset. Resident evidence for the regained count comes from guard, so one count per invocation keeps each regain exposed to a full guard before the next.

Proven failed marks are never retried within a session.

## Reset

- `reset --core N`: clears the core's failed mark, unproven depth and confirmation. The core restarts search from its baseline and the profile changes.
- `reset --all`: archives the session. The next `run` starts a new session with a fresh baseline and BIOS context.

`regain` and `reset` write to the journal, so they refuse while a `run` holds it (`journal.md`).

## Dead ends

shycler stops when it cannot make progress:

| Condition | Why it cannot continue |
|---|---|
| Attributed failure at offset 0 | The instability is not caused by Curve Optimizer. |
| SMU readback differs from the written value, or an SMU command fails | Offsets can no longer be trusted. |
| The same backend is inconclusive 3 times in a row, or is missing | No evidence can be produced. |
| 3 stray crashes in a row | The machine crashes before shycler acts: a boot loop. |
| A backend thread observed outside its allowed logical CPUs | Attribution is broken. |
| Preflight fails, including a changed BIOS context | The environment is not the one being tuned. |

What a dead end does in each run mode is in `runtime.md`. Thresholds are configurable.

A dead end follows from evidence recorded in the journal, not from memory, so a kill between the evidence and the `deadend` event still stops the next `run` before any SMU write. The evidence:
- an `smu.error`, or an `smu.readback` whose offset differs from `expected`;
- a `trial.end` with `escaped` CPUs;
- a backend's streak of inconclusive `trial.end`s reaching the threshold, not counting trials interrupted by a stop or restart;
- the stray-crash streak reaching the threshold;
- a failed `preflight.check`.

A `deadend` event consumes the evidence it reports: the SMU flag, the escape flag, every inconclusive streak or the stray streak. The other conditions are evaluated fresh by the following `run`. A failure at 0 is different: it leaves failed mark 0, so every later `run` stops again until `reset`.

## Tiers and certificate

Tiers rank the current profile by durability, not proof. Any profile change drops the tier until Bronze is earned again.

| Tier | Requirement |
|---|---|
| Bronze | Every core confirmed, and one clean rotation since the last profile change. |
| Silver | Bronze, and 24 clean hours. |
| Gold | Bronze, and 100 clean hours. |
| Platinum | Gold, and 200 field hours: real use observed by the future `observe` service with the BIOS offsets equal to the profile. Unavailable until `observe` exists. |

Each regime `r` with clean hours `T_r` shows its failure-rate bound: with zero failures, the rate is below `3 / T_r` per hour at 95% confidence (rule of three). The overall bound uses all clean hours.

The certificate shows:
- the tier, and the profile as a per-core table of edges;
- clean hours and failure-rate bound per regime, and the highest Tctl across counted trials;
- unproven depth per core;
- the BIOS context and session start;
- the SHA-256 of the journal up to the moment the certificate was rendered.
