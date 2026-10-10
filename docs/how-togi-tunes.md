# How togi tunes

This is the entry point to the tuning sequence. It explains the sequence and its cost; [the tuner spec](spec/tuner.md) owns the rules, [the workload spec](spec/workloads.md) owns what runs, and [the glossary](../GLOSSARY.md) owns the terms. The two-phase sequence below is the design of [#493](https://github.com/shgew/togi/issues/493) ([ADR 0054](adr/0054-two-phases-replace-the-deepening-loop.md)); the measured tables under [How long does it take?](#how-long-does-it-take) describe ruleset 10, before it.

Togi searches for negative Curve Optimizer offsets, from 0 to −50 counts. More negative is **deeper**. A **profile** is all cores' offsets. It deliberately encounters failures, including crashes; a clean cycle is evidence of the workloads tested, not a promise that the machine cannot fail later.

## The sequence

```text
Read the session baseline (or resume its journal)
        |
Search each core alone: R1 then R2, coarse then fine
        |
Check each candidate solo limit: five passes per class
        |
Apply all cores' offsets together, one count shallower
than each solo limit (the margin)
        |
PHASE 1: checking cycle across R1–R7 <---------------+
        |                                           |
        +-- failure --> attribute or hunt           |
        |                |                          |
        |             backoff --> rerun ------------+
        |             (only ever shallower)
        |
First passed full cycle: the first confirmed profile
        |
PHASE 2: rounds, while a core is shallower than its solo limit
        |   each round: one count toward the solo limit, check the
        |   moved cores; a failure blames a deepened core and rolls back
        |
Confirmation cycle (always runs, even when no core could move)
        |
        +-- failure --> deepened loaded cores return to their
        |               phase-1 offsets, then the cycle continues
        |
First passed full confirmation cycle: the confirmed profile
        |
Clean cycles count from here. Stop after --cycles N clean cycles,
or keep checking, never deepening again
```

A **trial** is one launch of one workload, under one fixed applied profile and one loaded set, for a specified duration. It passes, fails or is inconclusive. An inconclusive trial contributes no stability evidence; its retry follows the [scheduling rule](spec/tuner.md#scheduling) and is dropped if its context ends or its requirement changes. Five passes by default means five launches, not five minutes or five backend messages. The [pass rule](spec/tuner.md#evidence) tests a chosen failure-rate bound: with independent trials each failing with probability 0.5, five passes have probability 0.03125, below the configured miss probability 0.05. These defaults are a heuristic, not a fitted failure model or a guarantee about rare failures.

### Search: a core alone

Only the target core has its offset; every other core is at 0. A step is a 90-second R1 trial followed by a 90-second R2 trial. R1 exercises light single-thread boost; R2 exercises vector-heavy loads. The first failure rejects the step. Cores take turns so each can cool between its trials.

Before finding a failure, search moves five counts deeper after a passing step. Once it brackets a failure, it moves one count at a time. The candidate solo limit is −50 or the passed offset immediately shallower than the failure point. It then needs five passes in each of its frozen R1 and R2 classes. Ordinary search-step passes before that check do not count; eligible carried passes can count. [Search](spec/tuner.md#search) has a worked example and the exact rules.

The five-then-one stride and 90-second budget are heuristics: coarse steps reduce the number of launches before a failure is bracketed, then unit steps find the count boundary. There is no recorded comparison establishing them as optimal. Starting at the session baseline rather than at −50 is deliberate: [ADR 0006](adr/0006-start-from-bios-values.md) preserves the operator's chosen starting values unless configuration overrides them. BIOS CO 0 is recommended, not enforced.

### Checking: all offsets together

A **checking cycle** is one pass through a captured schedule. Its requirements are passing trials in particular classes. The default schedule covers all seven regimes, not just the R1/R2 loads used in search:

- R1 light; R2 vector heavy; R3 load steps; R4 medium duty; R5 both SMT threads; R6 idle and bursts; R7 multi-core vector-heavy loads.
- R1, R2 and R7 occur three times to cover their three workload catalogs. R3 and R4 also occur three times since ruleset 10 ([#107](https://github.com/shgew/togi/issues/107)). R5 occurs once; R6 occurs twice.
- Each R7 occurrence tests each CCD's full load, then a request-ordered chain of partial loads, then the all-core load. A partial idles the preceding load's top-request group, including ties, until fewer than two loaded cores would remain. Each part requires three short trials and one long trial.

The breadth is intentional ([ADR 0020](adr/0020-hunt-and-refine.md)); the precise composition, two R6 occurrences, and three-short-plus-one-long budgets are heuristics, not a measured optimum. Repeated short launches sample onset failures; the long launch adds sustained exposure. [Durations and checking schedule](spec/workloads.md#durations-and-checking-schedule) owns the allocations.

**A failure or backoff does not restart or fail the cycle.** It invalidates contradicted evidence, repairs the profile and reruns the failed class. Uncontradicted passes on deeper profiles can still cover the now-shallower profile. The same cycle resumes its remaining requirements. It can pass after dozens of failures and backoffs. For example, cycle 1 can fail an R7 part, hunt and back off, rerun that part, and later end passed—all still cycle 1.

After a profile change, every R7 chain re-derives from its full load. A part whose loaded set is unchanged keeps its passes and runs no new trial; a part whose set changed needs its own three short and one long trial; a set that left the chain is no longer required. The cycle therefore passes only after the ending profile's riskiest loads have run ([ADR 0055](adr/0055-rederive-r7-chains-after-a-backoff.md), [Together trial sequence](spec/tuner.md#together-trial-sequence)).

A **passed cycle** has fulfilled its requirements. A **full cycle** additionally has the required workload breadth. A **clean cycle** is a passed full cycle that ended at or after phase 2's conclusion. A passed cycle that is not clean ended before that, as phase 1's first confirmed cycle does, not because its trials failed. The exact rules are in [Checking](spec/tuner.md#checking).

### Hunts: identify an unattributed cause

A **hunt** repeats the failed workload and loaded set while changing which candidate cores retain their failing offsets. Other cores are held at parked offsets. This separates changing an offset from changing the load. Parts and complements narrow a failing subset; each group needs five passes by default and its first failure rejects it. Valid existing evidence can avoid launches ([ADR 0023](adr/0023-hunts-that-converge-on-shared-voltage.md), [ADR 0024](adr/0024-schedule-from-uncontradicted-evidence.md), [ADR 0027](adr/0027-carry-trial-facts.md)).

A hunt keeps its partition order across profile changes and resume. A crash while applying a group's parked profile is handled as parked, even before its trial starts; attribution naming one core ends the hunt naming that core ([Hunt](spec/tuner.md#hunt), [Crashes](spec/tuner.md#crashes)).

A singleton records a failure point. A failing multi-core subset records a combination: those offsets must not all be reached together. Member probes move each member separately to find usable backoff choices. A fallback combination records the conservative outcome when an ordinary hunt cannot reproduce a narrower failure; it is not proof that every member caused it.

Ordinary hunts handle unattributed failures outside multi-core R7, including idle crashes. Ruleset 10's **located hunts** test whether a multi-core R7 failure still occurs with unloaded cores at 0. They come from [#415](https://github.com/shgew/togi/issues/415) and [ADR 0040](adr/0040-located-hunts.md), not #107. [ADR 0053](adr/0053-escalation-only-located-hunts.md) makes them an escalation: such a failure is first charged by voltage-targeted backoff, and located only after its load has been backed off twice since its last passing trial or when no loaded core can step back. The exact partition order, evidence windows and outcomes are in [Hunt](spec/tuner.md#hunt).

### What happens after an R7 failure?

There is no tolerated 5% R7 failure rate. Every applicable multi-core R7 failure requires a move or establishes a dead end, except that after phase 2 concludes, an eligible failure may be held and moves nothing until a second one pairs with it; the gate, the exceptions and the display are in [Later failures](spec/tuner.md#later-failures); a historical proposal to tolerate failures was rejected ([ADR 0038](adr/0038-self-sufficient-cores.md), [#386](https://github.com/shgew/togi/issues/386)).

The loaded cores share a voltage rail. Attribution groups are per CCD: a named core is charged directly; otherwise the affected CCD's highest requesters are the initial candidates for a voltage-targeted backoff. `stalled_core` selects a CCD, not a culprit core. The global rail maximum and per-CCD groups are distinct. Being reported **self-sufficient** means a core has passed as a top requester for that workload; it is evidence, not proof of future stability, and it does not excuse failures.

| Situation | Next action |
|---|---|
| Live unattributed failure, with an unloaded core off CO 0 | Charge the loaded cores by voltage-targeted backoff, as in the rows below. Its reason says which backoff of this load it is since the load last passed. |
| Such a failure that [escalates](spec/tuner.md#escalation-to-a-located-hunt): its load was backed off twice since it last passed, or no loaded core on the affected CCDs can step back | Locate it. Keep the loaded cores at their failing offsets; put unloaded cores at 0. Do not move a loaded core while the hunt is open. |
| Locate fails | End `loaded`; charge the loaded-side failure. A group failure naming a loaded core charges that core. |
| Locate passes | Narrow the unloaded candidates with parts and complements; a culprit or combination backs off those candidates. If no narrower group fails, rerun the full failing profile at the failed duration before deciding `loaded`. |
| Failure charged to loaded cores | After phase 2 concludes, an eligible failure may be held instead and move nothing ([Later failures](spec/tuner.md#later-failures)). Otherwise choose a movable core in the affected request group, or the named core, and raise its request toward an eligible passing voltage target; without one, move one count shallower. Record its failure point. |
| Unattributed top group at CO 0 | Step down the affected CCD's request order to a group with a movable loaded core. An all-core failure does not charge an all-zero CCD while another affected CCD can move. |
| No movable affected loaded core, or a named top requester fails at 0 | Before stopping, rerun the same failed class with **every core** at 0, unless it already failed all-zero. A failed rerun establishes `failure_at_zero`; a passed rerun redirects attribution toward cores off CO 0 instead. When only unloaded cores are off 0, an all-zero locate performs this test. |
| Backoff changes the tuning profile | Rerun the failed class: five short trials, then one original-duration trial if different, with eligible evidence reusable. Continue the open cycle. |

This is the common path, not a second definition of edge cases. Carried failures, pending failures whose selected core is already shallower, CO-0 named non-top requesters, and all-zero-locate outcomes follow [R7 attribution](spec/tuner.md#r7-request-order-and-attribution), [voltage-targeted backoff](spec/tuner.md#r7-voltage-targeted-backoff) and [Dead ends](spec/tuner.md#dead-ends). The conversion of 3.6 mV per count used for estimated request shifts is an assumption, not a measured constant established for every core and operating point.

### Phases: take the margin back, then confirm

A **failure point** forbids that core's failing offset and deeper values. A **combination** forbids reaching all its members' recorded offsets together. These constraints apply to the tuning profile; a hunt deliberately tests a failed group profile.

Checking starts each core one count shallower than its solo limit, the **margin**. The margin is a method constant of one count, not fitted to any machine; it avoids failures that would each cost a step back. **Phase 1** is checking from those offsets until the first passed full cycle. It never deepens: group failures only step cores back. That cycle's profile is the first confirmed profile.

**Phase 2** follows by default. Its candidates are the cores shallower than their solo limit, whether by the margin or by a backoff of several counts. Each round moves every candidate one count toward its solo limit through offsets that reach no recorded failure point or combination, and checks only the moved cores' R1/R2 classes and affected R7 parts, with five short passes per class by default. A recorded failure point is never retried, so phase 2 can only take back margins and voltage-targeted backoffs; it barely deepens. When a round fails, the deepened core the failure blames backs off and every other mover, loaded or not, yields to its round baseline (the shallower of its last passed offset and its offset when the round began) and is finished: no later round moves it, so every core tries each offset at most once. Rounds are bounded: at most the largest gap plus the number of rounds that end unpassed.

Phase 2 always ends with one full confirmation cycle, also when no core could move. A failure in it returns a loaded deepened core to its phase-1 offset first. Its first passed full end concludes phase 2; checking then continues without ever deepening again, and failures only step cores back. Once the profile has passed a cycle after the conclusion, a core steps back on its second failure within 5 cycles, not its first, so single background failures do not wear the profile down; the BIOS profile does not wait: it shows a held failure's core already stepped back, marked unconfirmed, and returns to the held offset if 5 cycles pass with no second failure. [Phase 1](spec/tuner.md#phase-1), [Phase 2](spec/tuner.md#phase-2) and [Later failures](spec/tuner.md#later-failures) own the rules; [ADR 0054](adr/0054-two-phases-replace-the-deepening-loop.md) and [ADR 0056](adr/0056-later-failures-step-back-in-pairs.md) record why.

With `run --cycles 1`, togi stops right after the confirmation cycle; a larger N counts further cycles of indefinite checking. Without `--cycles`, checking continues. Stopping and resuming uses the journal; the session can span many reboots.

## How long does it take?

### One default cycle on 16 cores, two full eight-core CCDs

These are scheduled trial-duration totals with no failures, reruns or inconclusive retries, not real-machine wall-clock predictions. Request ties determine the R7 chains. The numbers are recorded in [#493's grilling measurements](https://github.com/shgew/togi/issues/493#issuecomment-6040930168).

| Cycle work | Trials × duration | Trials | Load-hours |
|---|---|---:|---:|
| R1, R2, R3, R4: three occurrences each | 4 × 3 × 16 × 120 s | 192 | 6.40 |
| R5: one occurrence per core | 16 × 120 s | 16 | 0.53 |
| R6: two occurrences | 2 × 900 s | 2 | 0.50 |
| R7: three occurrences, full CCDs and all cores | Per occurrence: 3 parts × 3 × 120 s, plus 300 + 300 + 600 s | 36 | 1.90 |
| R7 partial chains | 0–12 additional parts per occurrence, each 3 × 120 + 300 s | 0–144 | 0–6.60 |
| **Total** | | **246–390** | **9.3–15.9** |

Each eight-core CCD can supply up to six partials (seven down to two loaded cores), or none when top-request ties end the chain at once. Startup, teardown, crashes, hunts, backoffs and phase-2 rounds add time. Search and hunts have variable numbers of steps/groups; a search step is two 90-second trials, a candidate check is five 90-second trials per class, a hunt group up to five trials at its selected duration, and a phase-2 round class five 120-second trials. Their rules determine how many are needed; they are not fixed-length phases.

### Measured simulated sessions

The following are **simulated**, not hardware measurements: `target-shared-voltage`, 24 seeds pooled, as recorded on 2026-10-07 in [#493's time-per-phase and convergence tables](https://github.com/shgew/togi/issues/493#issuecomment-6040930168). Ruleset 10 used commit `645f5296`; ruleset 9 used `84c2a011` with the common simulator and fitted anchor. The anchor was regenerated in [#461](https://github.com/shgew/togi/pull/461), merge `bf7230d7`. [Benchmarking](benchmarking.md) documents reproduction and why earlier gate numbers used a different anchor.

| Activity | R9 median hours | R10 median hours | R10 p90 hours | R10 trials |
|---|---:|---:|---:|---:|
| Search | 13.4 | 13.4 | 13.5 | 553 |
| Located hunt: locate | — | 16.5 | 22.3 | 474 |
| Located hunt: parts and complements | — | 39.9 | 65.0 | 930 |
| Located hunt: member probes | — | 28.3 | 50.9 | 738 |
| Located hunt: full rerun | — | 4.1 | 6.1 | 124 |
| Ordinary hunts | 0.4 | 0.2 | 0.6 | 6 |
| Deepening rounds | 0.4 | 22.8 | 40.5 | 689 |
| Cycles passed, then reopened | 10.6 | 84.7 | 153.1 | 2136 |
| Final clean cycle | 11.6 | 14.5 | 15.1 | 358 |
| Backoff reruns | 12.4 | 16.1 | 20.2 | 519 |
| Crash reboots | 4.3 | 10.7 | 12.5 | — |
| **Total** | **53.1** | **249.8** | **390.8** | **6529** |

Each cell is a per-run statistic; medians of separate activities need not sum to the median total. R7 accounted for 158.5 simulated hours, **63%** of ruleset 10's time in the source analysis. The median final clean cycle was 14.5 hours, but most of the session came before it. The source recorded a median six cycles before the final clean one versus one for ruleset 9; no cycle in 252 across the analyzed 64 runs ended unpassed. That is why "not clean" must not be read as "failed cycle".

| Whole profile's convergence to its own final offsets | R10 median hours | R9 median hours |
|---|---:|---:|
| Within ±2 counts on every core | 203.1 | 37.7 |
| Within ±1 count on every core | 232.1 | 42.5 |
| Exactly final | 241.2 | 46.5 |

These are retrospective comparisons with each run's eventual profile, not an online stopping rule or accuracy against a known physical limit. On this anchor, ruleset 10's median total time was about **4.7×** ruleset 9's (249.8 / 53.1), not the pre-#461 gate's 1.90× figure.

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

Historical ADRs keep their original terms and decisions. The [ADR index](adr/README.md#adr-0020-terms-today) maps the old vocabulary to current rules; follow its supersession links rather than treating every old decision as active.
