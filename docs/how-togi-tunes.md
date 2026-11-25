# How togi tunes

This is the entry point to the tuning policy. [Ruleset 14](spec/tuner-ruleset14.md) owns the rules and numbers, [workloads](spec/workloads.md) owns what runs, and [ADR 0094](adr/0094-two-ends-bounded-exploration.md) records why two-ends D replaces statistical-cap permission.

This guide describes the shipped per-CCD policy. Automatic per-core tuning is the goal; the [edge–margin–wait model](model.md) is the design being built.

Togi searches for negative Curve Optimizer offsets, from 0 to −50 counts. More negative is **deeper**. It deliberately encounters failures, including crashes; passing the workloads tested is evidence about those workloads, not a promise that the machine cannot fail later.

## Two-ends D: Ruleset 14

Ruleset 14 is the shipped policy. Clean daily observation and exact-profile qualification can permit a bounded step beneath a night-derived ceiling. Completing those conditions does not establish a crash rate or certify the profile as stable. The [activation decision](spec/tuner-ruleset14.md#9-safety-and-activation) distinguishes a real-machine acceptance check from successor scoring.

## Days and nights

- **Nights test.** An operator starts the tuning boot or runs `togi run` by hand. There is no scheduled end time. Crash reboots can resume work; see [starting and ending a tuning boot](howto.md#4-overnight-the-tuning-boot).
- **Days apply and observe.** The day service applies the permitted committed profile and verifies its readback, without starting test workloads. See [daily observation](howto.md#daily-observation).
- **BIOS stays at CO 0.** togi applies offsets after boot. Before nonzero night work the operator must attest that the machine is stable at stock under the [recorded firmware context](spec/tuner.md#firmware); the night does not test an all-zero profile.

A profile is the complete ordered per-core vector and its unit mapping. The shipped policy tunes one CCD at a time, not individual cores.

## D, C and U

| Name | Meaning |
|---|---|
| **D**, day offset | Applied and read-back offset, distinct from saved or proposed values |
| **C**, hard night ceiling | Five counts shallower than the isolated edge, capped at stock |
| **U**, night edge | Tagged unknown, observed failure or write-limit-censored edge |

Unknown U grants no ordinary nonzero promotion. Statistical cap S computation and display are retired; historical `ccd.cap` journal records remain readable until the clean journal boundary.

## What a night does

A night works through four queues, with soft shares of elapsed observed time; [Night order and reserved budgets](spec/tuner.md#night-order-and-reserved-budgets) owns their priority, shares and when work is due.

| Queue | What it does |
|---|---|
| reproduction | Investigates a CPU-suspect daily event at its failed profile and with each CCD alone |
| qualification | Completes the coverage certificate for the next day profile |
| mapping | Finds each unit's isolated U and sweeps uncovered cells toward C |
| requalification | Refreshes the coverage certificate for the current day profile |

When none of the queues is due, filler repeats tests at the current nonzero D. A night can end with coverage still outstanding; unavailable backends supply no credit. The [workload matrix](spec/workloads.md#coverage-matrix) defines the evidence needed and which cells can be reused. Coverage is limited to those workloads and conditions; it does not establish that desktop freezes, sleep/wake failures or early-boot failures will be caught.

## What a day does

Day promotions are decided only at an eligible cold boot and committed only after verified all-core readback. A same-boot service restart, sleep resume or pause ending reapplies the still-permitted committed D, never promotes. Pause and stop verify CO 0; a cold boot starting paused applies stock and remains ineligible to promote even after the pause ends. [Runtime](spec/runtime.md#ruleset-13-policy-boundary) owns these application and recovery lifecycles.

- **Hold and step.** Every promotion, including the first from verified stock, needs 50 clean observed hours at the exact applied profile and exact next-profile qualification. Move one unfinished unit toward C using the bounded half-distance rule; [§4](spec/tuner-ruleset14.md#4-a-clean-hold-earns-one-step) owns selection, sizing and blockers.
- **Stop and retreat.** An in-campaign CPU-suspect event vetoes the whole profile, undoes the newest successful step and permanently stops that unit. Further in-campaign events walk every unit's actual history toward stock; [§5](spec/tuner-ruleset14.md#5-newest-step-blame-stop-and-the-machine-allowance) owns attribution, provisional events and escalation.
- **Campaign allowance.** Two unique CPU-suspect events at nonzero campaign profiles consume the shared machine allowance, including provisional and post-finish events. Historical crashes from before the machine's first campaign enrollment still veto their profile, but never consume the allowance, stop a unit or need applied-step ancestry. Reclassification is shown separately, never refunded. Restart, reset, firmware change, new session and reboot never renew it; exhaustion stops exploration, not observation or safety work.
- **Safety first.** Adverse U/C, vetoes and context invalidation still force retreat or stock. Freeze blocks promotion only. Firmware restoration needs the exact previous full profile's new-context certificate and a fresh stock hold; it grants no new depth or reopened climb.

An unexplained unclean end raises a question; independent CPU machine-check evidence remains CPU-suspect regardless of the answer. Unanswered/unsure ends at nonzero D consume allowance provisionally. Answer changes do not erase independent evidence or historical charges.

## What the screens show

`togi watch` and `togi status` ask the tuner for these numbers; they calculate nothing themselves.

`togi watch` has a header (clock, tuning time, session age, failures, crashes), a stage line, a narration line, one table per CCD and lower panels. Press `?` for the on-screen help and `l` for the log.

- **Stage line.** `NIGHT since 22:13 · 0h30m` with the task, for example `mapping CCD 1 at -20`, or `DAY`. It names no end time. After the night ended it shows `LAST NIGHT` times.
- **Unit facts.** Applied D, hard ceiling C and tagged U, qualification and outstanding cells.
- **Night panel.** Current task, mapping/qualification progress and queue time shares.
- **Day panel.** Exact-profile hold, phase, latest successful step, held/finished/stopped reason, blockers, firmware, freeze/pause, events and vetoes.
- **Campaign.** Consumed/remaining allowance, provisional and subsequently reclassified counts, and post-finish events.

Status and watch distinguish applied/read-back, proposed and paused saved profiles and show current-profile observations beside stock totals. The evidence is **experimental**. The displayed fixed-rate model bound, when available, is not a measured rare-crash rate. Neither recorded hours nor completed coverage predict how long the climb will take, and a finished climb is not a stability verdict.

Earlier rulesets are recorded in [Ruleset history](#ruleset-history), the changelog and the [ADR index](adr/README.md). Historical ADRs keep their original terms and decisions; follow their supersession links rather than treating every old decision as active.

## Ruleset history

Merge commits below are where each numbered constant first reached `main`, not necessarily a PR whose title names the whole behavior change: several were stacks. Commit `84c2a011` is a ruleset-9 **comparison reference**, not its introducing merge. The reasons come from the linked decisions and [#493's historical summary](https://github.com/shgew/togi/issues/493#issuecomment-6040930168); they distinguish target-machine observations from simulator evidence.

The Rust constant starts its own numbering at 1; Ruleset 13 used `togi_tuner::RULESET` 3. Ruleset 14's compatibility switch is distinct from its design number.

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
| 12 | Covered-load location and carried-failure rules ([ADR 0064](adr/0064-locate-when-passes-clear-the-loaded-cores.md)); repeated-charge experiments ([ADR 0065](adr/0065-test-repeated-r7-charges.md)); short-crash target suppression ([ADR 0066](adr/0066-short-crashes-take-no-voltage-target.md)); own-CCD named targets ([ADR 0067](adr/0067-named-targets-stay-on-their-ccd.md)); bounded named backoffs ([ADR 0068](adr/0068-cap-named-non-crash-r7-backoffs.md)); yielded movers remain eligible ([ADR 0069](adr/0069-yield-without-finishing-phase-two-movers.md)); singleton probes ([ADR 0070](adr/0070-probe-reproduced-singleton-failures.md)) | [#684](https://github.com/shgew/togi/issues/684) collected repeated-blame, target-selection, premature-finish and repeated-hunt costs into one transition; the frozen forward score failed, so no Ruleset 12 performance gate was registered | [`212612b1`](https://github.com/shgew/togi/commit/212612b1) |
| 13 | Day observation and night testing ([ADR 0079](adr/0079-day-observer-night-tester.md)); per-CCD profiles ([ADR 0080](adr/0080-per-ccd-first.md)); measured-slope caps ([ADR 0081](adr/0081-margin-from-measured-slope.md)); BIOS stays at zero ([ADR 0082](adr/0082-bios-stays-at-zero.md)); questions after unclean ends ([ADR 0083](adr/0083-ask-after-unclean-ends.md)); fresh transition ([ADR 0084](adr/0084-a-fresh-ruleset-transition.md)) | The confirmed per-core profile froze in daily use, with little exact-profile exposure and no light-work/hot-neighbour coverage ([#1214](https://github.com/shgew/togi/issues/1214), [#1215](https://github.com/shgew/togi/issues/1215), ADR 0079); evaluation results are simulated, not hardware qualification | [`ff406fd6`](https://github.com/shgew/togi/commit/ff406fd6) |
