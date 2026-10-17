# Narration changes

Every production phrase [#904](https://github.com/shgew/togi/issues/904) changed, as it read before the audit and as it reads now, grouped by the code that produces it. The [rendered phrase catalogue](narration-phrases.md) accompanies this before/after list. Wire names, event kinds, journal fields, `togi_journal::SCHEMA` and `togi_tuner::RULESET` are unchanged: only human text moved, and history already recorded keeps the message it was written with. Numbers, ids and offsets in the examples are placeholders. A phrase that exists in several fitted forms lists each form.

Where the dashboard fits one sentence to a width, a form is listed with the width it serves: full, then compact, then the shortest. This change builds on [#958](https://github.com/shgew/togi/pull/958), the history, TURNS and HUNT fitting work for [#912](https://github.com/shgew/togi/issues/912). Shared HUNT prose is marked **#912** below; its attribution-preserving fitting stays intact.

## Principles

- **A core's next turn is not the session's next trial.** A search forecast says `core 08 searches -5 on its next turn; next trial: R1 light, core 01 alone at 0`. Before, one unqualified `next` served both, so `core 08 next -5 → next: R1 light on core 01 alone at 0` read as if the trial ran at -5.
- **Say what the evidence supports.** A hold does not back off, a restored profile is not "safer", a failure point is the shallowest offset ruled out (not always a failure on its own account), a hunt skipped for a held failure is not a known-failure skip.
- **Use the glossary term.** `offset`, `backend`, `logical CPU`, `baseline`, `combination`, `full cycle`, `top requester` as [GLOSSARY.md](../GLOSSARY.md) defines them, where this change touches the phrase. Phrases it left alone keep their old words; the sections below list them.
- **Agree in number.** `1 core left`, `1 more step`, `1 passing trial`, `1 torn byte`, `1 candidate solo limit`.
- **A label does not promise a release.** The idle-trial screen holds while the trial is in flight; the planned end is a plan, not a time the screen resumes.

## Journal messages (`togi-journal`, `payload/message.rs`)

These are the stored `msg` of newly written events. `togi events`, the journal view and the system journal show them as written, and `togi status` shows the ones it forwards (such as the in-flight and last-decision lines). Dashboard narration and forecasts are synthesized elsewhere (see `words.rs` below), not from these strings. The wire is unchanged.

### `tuner.decision`

The one template `core {core} {verb} at {from}; next {to} ({reason})` is replaced by one template per decision, each naming its own source and destination.

| Decision | Before | After |
|---|---|---|
| `step_deeper` | `core 07 passed R1+R2 at -32; next -37 (reason)` | `core 07 passed R1+R2 at -32; searches -37 on its next turn (reason)` |
| `backoff`, activity search | `core 07 failed at -32; next -31 (reason)` | `core 07 failed at -32; searches -31 on its next turn (reason)` |
| `check_solo_limit`, candidate differs | `core 08 checks its solo limit at -7; next -6 (reason)` | `core 08 checks candidate solo limit -6 (from -7; reason)` |
| `check_solo_limit`, candidate equals the source | `core 08 checks its solo limit at -6; next -6 (reason)` | `core 08 checks candidate solo limit -6 (reason)` |
| `deepen` | `core 03 deepened at -10; next -11 (reason)` | `core 03 deepened from -10 to -11 (reason)` |
| `yield` | `core 08 yielded at -6; next -5 (reason)` | `core 08 yielded from -6 to -5 (reason)` |
| `backoff`, other activities | `core 00 backed off at -25; next -24 (reason)` | `core 00 backed off from -25 to -24 (reason)` |
| `hold` | `core 03 holds at -24; next -24 (reason)` | `core 03 failure held; tuning stays at -24 (reason)` |

Rationale: the old sentence gave the source offset to verbs that read as describing the destination (`deepened at -10; next -11`), `next` did not say search turn or scheduled trial, and the candidate a solo-limit check tests is the destination, not the offset named. A hold names the held failure and the unchanged tuning offset, so it is not read against the stepped-back, unconfirmed BIOS profile.

### `session.carried`

| Before | After |
|---|---|
| `carried 1 candidate solo limits and 1 failure points from …` | `carried 1 candidate solo limit and 1 failure point from …` (each noun agrees on its own count; `limits`/`points` otherwise) |
| `carried 1 candidate solo limits from …; failure points stay behind: REASON` | `carried 1 candidate solo limit from …; failure points stay behind: REASON` (`limits` at any other count) |

### Offsets written as `CO`

The numeric setting is an **offset**, as the glossary says.

| Producer | Before | After |
|---|---|---|
| `smu.intent` / preflight command text (`set core`) | `set core 03 to CO -20` | `set core 03 to offset -20` |
| same (`set all`) | `set all cores to CO 0` | `set all cores to offset 0` |
| `smu.error` (`set core`) | `set core 03 to CO -20 failed: …` | `set core 03 to offset -20 failed: …` |
| `smu.error` (`set all`) | `set all cores to CO 0 failed: …` | `set all cores to offset 0 failed: …` |
| `trial.intent`, one offset | `trial 0413 core 03 CO -32 R2 …` | `trial 0413 core 03 offset -32 R2 …` |
| `trial.intent`, all-zero rerun | `… rerun with every core at CO 0` | `… rerun with every core at offset 0` |

The accepted audit corrections leave these existing message families unchanged: `SMU wrote core 03 CO -20`, `SMU wrote all cores CO 0`, `SMU core 03 reads CO -20`, `profile applied for alone trials: every core at CO 0`, `core 07 failure at CO -30: …`, and the tuner reasons that say `every core at CO 0` (`zero.rs`, `r7/respond.rs`, `r7/reason.rs`).

### `trial.intent` target

| Before | After |
|---|---|
| `trial 0604 cores 05 R1 …` (one loaded core) | `trial 0604 core 05 R1 …` |

`cores 03, 07` is unchanged for two or more.

### `checking.step`

| Before | After |
|---|---|
| `checking cycle 2 R7 step 4 starts at current profile …` | `checking cycle 2 step 4 starts at current profile …` |

The step's regime is not recorded in this event, and the old text claimed R7 for every step.

## Tuner reasons (`togi-tuner`)

| Producer | Before | After |
|---|---|---|
| `search.rs`, search exit reason | `solo limit -30 passed 5 trials of R1 W and R2 W; checking starts at …` | `solo limit -30 passed 5 trials each of R1 W and R2 W; checking starts at …` (`1 trial each` at one) |
| `r7/reason.rs`, backoff ordinal note | `not located on the unloaded cores yet: this is voltage-targeted backoff N of this load since its last passing trial, and its failures are located only after M` | `the unloaded cores have not been checked by a located hunt yet; voltage-targeted backoff N for this load since its last passing trial; a later failure can trigger a located hunt after M backoffs` |

Rationale: each of R1 and R2 needs the full quota, not a shared one; and the note explains when the tuner becomes eligible to start a hunt, not that a failure was already located.

## `togi status` (`crates/togi/src/status.rs`, `read.rs`)

| Producer | Before | After |
|---|---|---|
| Header, search activity | `search: 1 cores left` | `search: 1 core left`; `search: N cores left` otherwise |
| Header, missing full-cycle coverage | `R3: 1 more steps` | `R3: 1 more step`; `R3: N more steps` otherwise |
| Dead end line | `dead end: failure_at_zero [#N]` | `dead end: failure at offset 0 [#N]` |
| | `dead end: smu [#N]` | `dead end: SMU error [#N]` |
| | `dead end: no_evidence [#N]` | `dead end: no conclusive evidence [#N]` |
| | `dead end: boot_loop [#N]` | `dead end: boot loop [#N]` |
| | `dead end: containment [#N]` | `dead end: workload containment failed [#N]` |
| | `dead end: preflight [#N]` | `dead end: preflight failed [#N]` |
| | `dead end: defect [#N]` | `dead end: recorded decisions affected by a defect [#N]` |
| | `dead end: thermal_trip [#N]` | `dead end: thermal trip [#N]` |
| BIOS note, unconfirmed, several cores | `* unconfirmed since [#S]: cores 01 02 show a stepped-back offset, the profile confirmed by the passed full cycle [#C] otherwise; a passed full cycle confirms it, except a held failure's stepped-back offset, which stays marked until the hold ends` | `* unconfirmed since [#S]: cores 01 02 show stepped-back offsets; other BIOS offsets come from the profile confirmed by the passed full cycle [#C]. A passed full cycle confirms the changes, except a held failure's stepped-back offset, which stays marked until the hold ends` |
| BIOS note, unconfirmed, one core | `… cores 01 show a stepped-back offset, …` | `… core 01 shows a stepped-back offset; other BIOS offsets come from …` |
| R7 self-sufficiency cell | `observed (1 passing trials)` | `observed (1 passing trial)`; `N passing trials` otherwise |
| Hunt, parked-offset source | `parked offsets cycle end #N` | `parked offsets from cycle end #N` |
| Hunt, failure with no trial in flight (carried idle failure) | `hunt 1 [#N]: unattributed crash in R6 trial -; parked offsets all-zero` | `hunt 1 [#N]: unattributed crash with no trial in flight; parked offsets all-zero` |
| Hunt, failure in a trial | `unattributed crash in R6 trial 0412` | unchanged |
| Hunt group row, locate group | CORES cell `-` | CORES cell `locate`, with the note `locate: reruns the failed load with its loaded cores at their failing offsets and every unloaded core at 0` |
| Evidence table | column `TRIALS`, no heading | heading `Valid passes supporting the checking profile (live and not alone, on the active backend, at equal or deeper offsets; carried passes excluded)`, column `VALID PASSES` |
| `read.rs`, torn tail (also `events`, `watch`) | `journal ends with 1 torn bytes; the next run records journal.torn` | `journal ends with 1 torn byte; …`; `N torn bytes` otherwise |

The `in flight:` and `last decision` lines forward the recorded journal message, so they show the journal wording above for events written from now on.

Rationale: a dead end label is for a reader, not the wire token (the wire is unchanged); the unconfirmed note ties `otherwise` to the unaffected BIOS offsets; `trial -` hid that an idle failure has no trial; a locate group tests the loaded cores, not nothing; the evidence column counts live passes that still support the profile, not all trials.

## Dashboard words (`togi-watch`)

### `words.rs`: history sentences, forecasts and compact forms

| Producer | Before | After |
|---|---|---|
| Step deeper, history | `core 03 passed -10, next -15` | `core 03 passed -10; searches -15 on its next turn` |
| Step deeper, forecast | `core 03 next -15` | `core 03 searches -15 on its next turn` |
| Check solo limit, history | `core 03 confirming -15 as its solo limit` | unchanged |
| Check solo limit, forecast | `core 03 confirms -15` | `core 03 starts confirming -15 as its solo limit` |
| Yield, history | `core 03 -10 → -9 so others go deeper` | `core 03 yields -10 → -9; no later round moves it` |
| Yield, forecast | `core 03 yields -10 → -9` | `core 03 yields -10 → -9, back to its round baseline` |
| Hold, history | `core 03 holds at -10` | `core 03 failure held; tuning stays at -10` |
| Hold, forecast | `core 03 holds at -10` | `core 03 failure held at -10; no tuning move` |
| Unexpected exit | `the stress program quit` | `the backend quit` |
| Stall | `the stress program stalled` | `the backend stalled` |
| Between-trial MCE, no known core | `cpu 3 corrected error between trials (recorded only)` | `logical CPU 3 corrected error between trials (recorded only)` |
| Full cycle passed, history | `cycle 2 passed, a full cycle of every kind of test` | `cycle 2 passed, a full cycle` |
| Hunt skipped, history, no recorded reason | `not needed, these offsets already reach a known failure` | `not hunted` |
| Hunt skipped, history, with the recorded reason | same sentence for every skip | `not hunted: REASON` |
| Hunt skipped, detailed forecast | `no hunt: these offsets reach a known failure` | `no hunt`, or `no hunt: REASON` |
| Hunt skipped, compact forecast | (none) | `no hunt` |

These signal words are synthesized by the dashboard: trial-end history, the NOW band's last-trial line and the forecast chains. The journal view and `togi events` show the stored `event.msg` instead, which these words do not change.

Rationale: the old hunt-skipped text claimed a known failure for a skip the tuner recorded as held (`held: first unattributed failure …`); the recorded reason is now shown. A yield returns the core to its round baseline, which is the shallower of its last passed offset and the offset when the round began, and no later round moves it; it does not make other cores go deeper.

### Pause labels for an idle trial (`dash/header.rs`, `dash/stage.rs`, `dash/now.rs`)

| Producer | Before | After |
|---|---|---|
| Header, long | `screen paused until 10:10` | `screen paused · planned end 10:10` |
| Header, short | `paused until 10:10` | `paused · planned end 10:10` |
| Header, compact one-line form | `paused until 10:10` | `paused · planned end 10:10` |
| Stage line | `screen paused until 10:10` | `screen paused · planned end 10:10` |
| NOW band, centred message | `  screen paused until 10:10 so it cannot wake the idle cores  ` | `  screen paused · planned end 10:10, so it cannot wake the idle cores  ` |
| NOW band, clock label | `screen paused · ends at 10:10` | `screen paused · planned end 10:10` |

Rationale: the screen does not resume when the planned end arrives; it stays frozen while the trial is in flight, even past the planned end ([runtime.md](spec/runtime.md#commands)).

### Core notes (`notes.rs`, `dash/ccd.rs`)

| Producer | Before | After |
|---|---|---|
| Top-requester note, measured, wide | `top requester` | unchanged |
| Top-requester note, measured, narrow | `top` | unchanged |
| Top-requester note, offset fallback, wide | `top by offset` | unchanged |
| Top-requester note, offset fallback, narrow | `top` | `offset top` |
| Top-requester note, offset fallback, narrower than `offset top` | `top` | left out |

Rationale: `top` shown for an offset proxy read as a measured request.

### Help (`help.rs`)

| Item | Before | After |
|---|---|---|
| READING THE SCREEN, `▬` | `its failure point: this core failed there on its own account` | `its failure point: the shallowest offset ruled out for this core` |
| PHASE 1 | `The first full cycle, run on the profile the solo limits give.` | `Checking toward the first confirmed profile. The stage shows whether phase 1 is open or finished.` |
| WORDS, failure point | `the shallowest offset where a core failed on its own account` | `the shallowest ruled-out offset for a core` |
| WORDS, parked | `a core a hunt holds shallower than its failing offset` | `a core a hunt holds no deeper than its failing offset, outside the selected group` |
| WORDS, top requester | `the loaded core with the highest voltage request, setting the shared core voltage; marked top on the CCD tables` | `an R7 workload's highest-request group on each CCD, ties included; marked top on the CCD tables` |

Rationale: a failure point can come from R7 voltage-targeted backoff, a hunt culprit or a carried failure, so it is a ruled-out depth, not always a failure on the core's own account; phase 1 ends at the first passed full cycle after every core left search, not at the first cycle; a parked offset can equal the failing offset; the CCD table marks per-CCD top groups, which can differ from the core that sets the shared rail ([GLOSSARY.md](../GLOSSARY.md#top-requester)).

### BIOS panel (`panels/search.rs`)

| Producer | Before | After |
|---|---|---|
| Footnote under starred offsets, panel at least 72 cells wide | `* not confirmed: a passed cycle confirms it, unless held` | `* unconfirmed: a passed full cycle confirms it, unless a failure is held` |
| Same, narrower panel (56 cells with the star) | same text | `* a passed full cycle confirms, unless a failure is held` |

Rationale: only a passed full cycle confirms the BIOS projection, and the old `held` did not say what was held. The narrow form drops `unconfirmed:` because the heading and the star already say it.

### HUNT panel (`panels/hunt.rs`) — **#912**

| Producer | Before | After |
|---|---|---|
| Candidates | `00 01 02, every core with an offset in the failing profile` | `00 01 02, deeper at failure than at their parked offsets` |
| Candidates, shortened fitted form | `00 01 02, every core at a failing offset` (from #958) | `00 01 02, deeper at failure than parked` |
| Part schedule, after the first part | `then finer parts and their complements, until one fails or none does` (`then finer parts and complements until one fails or none does` after #958) | `failed parts are split again; complements test what remains` |
| Then block, heading | `if the hunt resolves and the rerun passes:` | `after the hunt:` |
| Then block, resume step | `resume cycle 2` | `if the rerun passes, resume cycle 2` |

Rationale: every core has an offset in the profile, and a hunt's candidates are those deeper at failure than parked (`tuner.md`, Hunt); a failed part restarts splitting rather than finishing the hunt; passing the rerun conditions the resume, not recording the result or the backoff that precede it.

### Story (`frames/story`)

| Producer | Before | After |
|---|---|---|
| All-zero rerun, label (also in the operation heading) | `RERUN AT CO 0` | `RERUN AT OFFSET 0` |
| All-zero rerun, line 1 | `A failure at CO 0 would stop tuning. Before it does, the failed load runs again with every core at CO 0.` | `A failure at offset 0 would stop tuning. Before it does, the failed load runs again with every core at offset 0.` |
| All-zero rerun, line 2 | `If it fails too, … the failure goes to the cores off CO 0.` | `If it fails too, … the failure goes to the cores off offset 0.` |
| All-zero rerun, brief | `The failed load runs again with every core at CO 0 before tuning stops.` | `The failed load runs again with every core at offset 0; tuning stops if it fails.` |
| Idle trial, full | `This screen holds still until 00:31, clock included, so drawing it cannot wake the cores.` | `This screen holds still while the idle trial is in flight, clock included, so drawing it cannot wake the cores.` |
| Idle trial, brief | `Every core idle with short wake-ups; this screen holds still.` | unchanged |
| Stopped after cycles, saved | `The requested clean cycles are complete, so I restored safer offsets and stopped. …` | `The requested clean cycles are complete, so I restored baseline or shallower offsets and stopped. …` |
| Stopped, other reasons, saved | `I restored safer offsets before stopping. …` | `I restored baseline or shallower offsets before stopping. …` |
| Dead end, no evidence | `I couldn't obtain usable failure evidence, so I can't continue testing safely. …` | `I couldn't obtain conclusive trial evidence, so I can't continue testing safely. …` |
| Member probe | `So I probe each member: how shallow must it go for the rest to pass? Core 01 first.` | `So I probe each member: how shallow must it go for the combination to pass? Core 01 first.` |

Rationale: the screen holds by the trial being in flight, not by the clock; the brief rerun form claimed an unconditional stop; a restoration writes the baseline or the current offset where shallower, which is not known to be safer ([runtime.md](spec/runtime.md#exit-codes)); the evidence missing may be a conclusive result, not a failure; the probed member is part of the combination whose passing is asked about.

The Containment dead end still says `stress programs` (`story/mod.rs`); the audit proposed no correction to that branch.

### Story outcome forecast (`outcome.rs`)

| Producer | Before | After |
|---|---|---|
| Next trial prefix, everywhere | `next: trial 2 of 4` | `next trial: trial 2 of 4` |
| | `next: this load again` | `next trial: this load again` |
| | `next: trial 3 of 4 · 5m` | `next trial: trial 3 of 4 · 5m` |
| | `next: step 2: …`, `next: part 2: …`, `next: hunt N group M: …`, `next: rerun …` | `next trial: step 2: …`, `next trial: part 2: …`, `next trial: hunt N group M: …`, `next trial: rerun …` |
| Join between a decision chain and the next trial | `core 08 next -5 → next: R1 light on core 01 alone at 0` | `core 08 searches -5 on its next turn; next trial: R1 light, core 01 alone at 0` (decisions still join with ` → `) |
| Single-core next trial | `R1 light on core 01 alone at 0`; `R1 light on core 01 at -16` | `R1 light, core 01 alone at 0`; `R1 light, core 01 at -16` |
| Multi-core next trial | `R1 light on 00-02` | unchanged |
| Hunt part with an empty layout | `next: part 1: ` (nothing after the colon) | `next trial: part 1: WHAT` (falls back to the trial's own description) |
| All-zero rerun, compact | `→ rerun at CO 0` | `→ rerun at offset 0` |
| Full row label | `if it passes` padded to 21 cells | `if it passes:` padded to 21 cells (a long label keeps one gap cell) |
| Tightest form of a single-core next trial | `→ R1 on core 03` shortened to its digits and target | `→ R1 core 03` |

The compact and tightest forms are `pass → R1 light, core 01 alone at 0` and `pass → R1 core 01`, so a narrow screen still keeps the target.

Rationale: [#904](https://github.com/shgew/togi/issues/904)'s owner decision, above. The consumers of the text (`compact_next`, `tight_next`, `fit_phrases`) read the new shape, so fitting cannot turn it back into the ambiguous one.

### Resting band (`resting.rs`)

| State | Before | After |
|---|---|---|
| Between trials, recovery | `next: deciding` | `next trial: deciding` |
| | `next: read the core ranking before deciding` | `next trial: read the core ranking before deciding` |
| | `next: step 2: R7 all-core with W` | `next trial: step 2: R7 all-core with W` |

## Where the wording is documented

- [GLOSSARY.md](../GLOSSARY.md): the all-zero rerun's `offset 0` and its `RERUN AT OFFSET 0` label.
- [docs/spec/runtime.md](spec/runtime.md): the `status` command (singular agreement is not specified; the locate group, `VALID PASSES`, plain-word dead end) and the search-forecast paragraph.
- [docs/spec/journal.md](spec/journal.md): the sample `trial.intent` and `tuner.decision` messages and the per-decision wording.
- [docs/howto.md](howto.md): the all-zero rerun message.
