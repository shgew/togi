# How togi tunes

This is the entry point to the tuning policy. It explains the shape of the policy and what the screens show; [the tuner spec](spec/tuner.md) owns the rules and numbers, [the workload spec](spec/workloads.md) owns what runs, and [the glossary](../GLOSSARY.md) owns the terms. The design is [#1214](https://github.com/shgew/togi/issues/1214) with the owner's answers in [#1215](https://github.com/shgew/togi/issues/1215); [ADR 0079](adr/0079-day-observer-night-tester.md), [ADR 0080](adr/0080-per-ccd-first.md) and [ADR 0081](adr/0081-margin-from-measured-slope.md) record the decisions.

Togi searches for negative Curve Optimizer offsets, from 0 to −50 counts. More negative is **deeper**. It deliberately encounters failures, including crashes; passing the workloads tested is evidence about those workloads, not a promise that the machine cannot fail later.

## Days and nights

- **Nights test.** The operator starts a night (the tuning boot, or `togi run` by hand). It tests with workloads and runs until the operator ends it or the next normal boot begins; it has no end time. Ending it restores every core to CO 0.
- **Days observe.** In normal boots togi applies one profile and watches it without running workloads: applied hours, faults, clean and unclean ends. [Daily observation](howto.md#daily-observation) says how to answer what happened after an unclean end.
- **BIOS stays at CO 0.** Togi applies the day profile at boot ([ADR 0082](adr/0082-bios-stays-at-zero.md)); it never tells you to enter offsets in BIOS.

A **profile** is one offset per CCD, expanded to every core of that CCD. Per-core refinement is outside this ruleset.

## Three numbers per CCD

| Name | Meaning | Who moves it |
|---|---|---|
| **D**, the day offset | The offset applied on normal boots; 0 before the first step | Day steps, retreats and a firmware reset |
| **S**, the cap | The deepest offset the night evidence supports | Night evidence, recomputed whenever it changes |
| **U**, the edge | The shallowest offset that actually failed with only that CCD tuned | Night mapping and reproduction, never a day event |

**U** is *unknown* until mapping has looked, *fails at N* once a failure is observed, or *censored at −50* when nothing failed down to the write limit. Unknown is not zero.

**S** is the shallower of two limits. One is the *rate limit*: togi budgets one added crash per 3,000 applied hours for the machine, split equally between CCDs, and projects each mapping class's failure rate to the offset where it reaches that share. The other keeps a margin of five counts from the edge, `min(U + 5, 0)`. With no adequately exposed mapping class, or U unknown, S is 0. The Poisson rate and the log-linear projection are modeling assumptions, not a proof about rare failures.

The projection needs a **slope**: how fast the failure rate grows with depth. Until some class has failures at two or more offsets, togi uses a labelled **fallback** slope (a rate 1.5 times higher per count). After that the **measured** slope is the conservative end of a fitted interval, and later non-mapping evidence cannot make S deeper.

**Coverage.** Only mapping classes count towards S: single-core rotation, light core beside heavy neighbours, multi-core partial loads and all-threads serial positions. Everything else, such as load steps, duty-limited loads, SMT, idle, and qualification, control and reproduction exposure, is **uncovered for the cap** even when it passed. `togi status` lists those classes; uncovered is never read as safe.

## What a night does

A night works through five queues. Each has a **share** of the night's elapsed observed time. Shares are soft: a queue at or over its share yields only while another queue with due work is under its own.

| Queue | What it does | Share of elapsed observed time |
|---|---|---|
| reproduction | Re-runs the failed profile of the oldest CPU-suspect daily event, then each CCD alone; one night per event | up to 25% |
| qualification | Runs the whole coverage matrix on the exact next day profile | up to 75% |
| mapping | Descends each CCD alone from 0 by five counts, brackets the first failure in single counts, then sweeps from U towards S | 10% |
| control | Runs the whole matrix at all-zero on the first night and after a firmware change; a conclusive failure there stops togi with `platform unstable at stock` | 5% |
| requalification | Re-runs the matrix for the current D 30 days after its last full qualification, or at once when invalidated | 10% |

Queues are visited in the order reproduction, qualification, mapping, control, requalification. When none has due work the **filler** runs: a cap probe one count deeper than S on the CCD whose S is furthest from its edge, otherwise exact repeats of D's qualification cells. A cell that needs an **unavailable backend** (one that stayed inconclusive through its retries) is skipped and the night records it. A night ends at the operator's stop, the next normal boot, a dead end, or when every cell it could run needs an unavailable backend.

A **trial** is one launch of one workload under one applied profile for a stated duration. It passes, fails or is inconclusive; an inconclusive trial adds no evidence. Exposure is eligible measured execution time only, never intended time, downtime or an extrapolated crash tail. [The coverage matrix](spec/workloads.md#coverage-matrix) owns which classes a qualification needs.

A **qualification** is complete only when every cell of the matrix has been witnessed on the exact profile under current content, epoch and firmware. Reproduction never qualifies. A night can end mid-matrix; the next one resumes the outstanding cells.

## What a day does

Day steps are decided at a cold boot and applied only after a verified readback.

- **Hold.** Every promotion after the first needs 50 applied daily hours at the current exact D since the latest CPU-suspect event. Paused, suspended, unknown and tuning time does not count.
- **Step.** The CCD furthest from its S moves `max(1, ceil((D−S)/2))` counts towards S, never past it, and only to a profile that is qualified, not deeper than any S, not vetoed, with freeze off, no question pending and no unresolved event. A step that is not allowed is a *hold* that names its blocker.
- **Veto.** A CPU-suspect daily event, including a crash recorded by hand, forbids that whole profile and anything at least as deep on every CCD. A veto never expires and no clean night clears it; only reclassifying its event does. At the next cold boot the newest promotion is reverted, and a further event at the stepped-back profile reverts each CCD's newest remaining step.
- **Retreat.** If S becomes shallower than D, the day profile retreats without waiting for a hold. If the shallower profile is not qualified, D falls to CO 0 until it is.
- **Freeze.** Keeps D and blocks promotions; it never blocks a retreat or a firmware reset.
- **Firmware.** A BIOS, AGESA or microcode change invalidates qualifications, slopes and caps and sets D to CO 0. New controls and a new qualification must pass before a nonzero profile returns.

An unclean end raises a question; answering `frozen` makes it CPU-suspect, `graphics` or `power_off` makes it non-CPU, and `unsure` or no answer at a nonzero D forms a provisional CPU-suspect event. The rules are in [Daily events](spec/tuner.md#daily-events).

## What the screens show

`togi watch` and `togi status` ask the tuner for these numbers; they calculate nothing themselves.

`togi watch` has a header (clock, tuning time, session age, failures, crashes), a stage line, a narration line, one table per CCD and lower panels. Press `?` for the on-screen help and `l` for the log.

- **Stage line.** `NIGHT since 22:13 · 0h30m` with the task, for example `mapping CCD 1 at -20`, or `DAY`. It names no end time. After the night ended it shows `LAST NIGHT` times.
- **CCD table.** Its title gives the CCD's load state. The first line reads `D -10 · S -26 fallback slope · bound by rate R1 · U fails at -31 · 16 counts to S`; the second `3 covered for the cap · 1 uncovered for the cap`. Each core row gives its offset and a gauge: applied depth, dots up to S, and a red mark at U (amber when censored).
- **Night panel.** The open task, the mapping or qualification in progress, any reproduction, and each queue's `share` as used/target, such as `mapping 66%/10%`.
- **Day panel.** `hold` (applied hours of the hours a step needs), the last step, freeze, firmware validity, pending questions, events and vetoes.
- **Help.** Explains the words in the screen (offset, trial, D, S, U, slope, covered, hold, step, veto, freeze, qualifying, reproducing, share) and the kinds of load; the tuner spec owns the rules.

`togi status` prints the same facts as text: the activity line (`night since 00:00, 11h55m elapsed: mapping (task mapping #2)`, or the day, a stop or a dead end); session and BIOS context; the trial in flight; a table `CCD CORES D S SLOPE BINDING U REMAINING COVERED UNCOVERED` with the uncovered classes below it; a **Day policy** part (applied profile, hold, latest step and what blocks it, freeze, firmware, events and vetoes); a **Night** part with the elapsed observed time and the queue table `QUEUE SHARE USED TIME`, current task, mapping cursors and qualification progress; and **Day** observation totals. Neither screen estimates a completion time.

## Costs

A qualification runs the whole matrix on one exact profile, so each step costs a night or several. Time already spent never counts twice: a pass at an unchanged CCD can be reused where [the workload spec](spec/workloads.md#coverage-matrix) allows. The simulated benchmarks for this policy are scored by [the registered day/night gate](benchmarking.md#daynight-gate); this document states no measured duration.

Earlier rulesets are recorded in [Ruleset history](#ruleset-history), the changelog and the [ADR index](adr/README.md). Historical ADRs keep their original terms and decisions; follow their supersession links rather than treating every old decision as active.

## Ruleset history

Merge commits below are where each numbered constant first reached `main`, not necessarily a PR whose title names the whole behavior change: several were stacks. Commit `84c2a011` is a ruleset-9 **comparison reference**, not its introducing merge. The reasons come from the linked decisions and [#493's historical summary](https://github.com/shgew/togi/issues/493#issuecomment-6040930168); they distinguish target-machine observations from simulator evidence.

| Ruleset | What changed | What prompted it | Introducing merge commit |
|---|---|---|---|
| 1 | Build stamps and refusal to resume incompatible rulesets/schemas; existing solo search and confirmation ([ADR 0009](adr/0009-compatibility-across-updates.md)) | Compatibility/safety design, not a benchmark claim | [`675d3124`](https://github.com/shgew/togi/commit/675d3124) |
| 2 | Load-scoped blame for unattributed failures, wider solo confirmation, automatic regain after clean checking ([ADR 0011](adr/0011-blame-by-load-and-automatic-regain.md)) | Observed resident crashes and overly broad blame; rationale did not prove prevention | [`862badb2`](https://github.com/shgew/togi/commit/862badb2) |
| 3 | AVX-512 first in solo confirmation ([ADR 0015](adr/0015-avx-512-first-in-confirmation.md)) | Target machine: 14 of 17 confirmation failures occurred at that later slot, repeating earlier work | [`a568efc3`](https://github.com/shgew/togi/commit/a568efc3) |
| 4 | Candidate pass rule, hunts, combinations/member probes, globally targeted deepening, clean-cycle conclusion; replaced separate solo confirmation and automatic regain ([ADR 0020](adr/0020-hunt-and-refine.md)) | Target machine: 21.2 hours without a clean checking cycle; simulated combination probes | [`9d38bbf1`](https://github.com/shgew/togi/commit/9d38bbf1) |
| 5 | Parked-profile and combination-backoff improvements; reuse earlier part/complement evidence ([ADR 0023](adr/0023-hunts-that-converge-on-shared-voltage.md)) | Target machine: hunts of 42–49 groups and 2.3-hour repeats | [`36601701`](https://github.com/shgew/togi/commit/36601701) |
| 6 | Evidence-selected long hunt duration, partition hints and credit for earlier uncontradicted cycles ([ADR 0024](adr/0024-schedule-from-uncontradicted-evidence.md)) | Simulator/bench comparisons of repeated work | [`3093dbf6`](https://github.com/shgew/togi/commit/3093dbf6) |
| 7 | Carry decisive trial facts, skip known failures, remove Bronze/Silver/Gold tiers ([ADR 0027](adr/0027-carry-trial-facts.md), [ADR 0028](adr/0028-remove-tiers.md)) | Target machine: 160 repeated checks, about four hours | [`ef164455`](https://github.com/shgew/togi/commit/ef164455) |
| 8 | Record-only partial R7 loads ([#334](https://github.com/shgew/togi/pull/334)) | Target machine: CCD1 failures at about 7% per start; gather evidence without using partial outcomes for decisions | [`a498144c`](https://github.com/shgew/togi/commit/a498144c) |
| 9 | Request-ordered R7 chains, ordinary partial evidence, top-request attribution, voltage-targeted backoff and self-sufficiency reporting ([ADR 0038](adr/0038-self-sufficient-cores.md)) | Target machine: core 11 failed only without core 10 loaded; simulator gate | [`0ccbe089`](https://github.com/shgew/togi/commit/0ccbe089) |
| 10 | Located hunts, full failing-duration rerun, all-zero rerun before zero-offset dead ends ([ADR 0040](adr/0040-located-hunts.md)); backend-identity pass validity; default R3/R4 three times ([#107](https://github.com/shgew/togi/issues/107)) | Simulator: `idle-limit`, `late-onset`, `target` and `target-nonmember-mce` exposed incorrect blame/dead ends; broader load-step/duty coverage | [`c660ba67`](https://github.com/shgew/togi/commit/c660ba67) |
| 11 | Escalation-only located hunts ([ADR 0053](adr/0053-escalation-only-located-hunts.md)); crashes are a cost ([ADR 0052](adr/0052-crashes-are-a-cost.md)); one-way phase 1 with a one-count margin and a bounded phase 2 with a confirmation cycle ([ADR 0054](adr/0054-two-phases-replace-the-deepening-loop.md)); R7 chains re-derived after a backoff ([ADR 0055](adr/0055-rederive-r7-chains-after-a-backoff.md)); later failures step a core back in pairs ([ADR 0056](adr/0056-later-failures-step-back-in-pairs.md)); idle thermal-trip failures ([#494](https://github.com/shgew/togi/issues/494)); retry context ([#590](https://github.com/shgew/togi/issues/590)); fixed hunt order ([#591](https://github.com/shgew/togi/issues/591)); hunt-duration escalation ([#507](https://github.com/shgew/togi/issues/507)) | Simulator: 249.8 h median on `target-shared-voltage` under ruleset 10 ([#493](https://github.com/shgew/togi/issues/493)) | [#683](https://github.com/shgew/togi/pull/683) |
