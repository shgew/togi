# Tuner

Normative rules for Ruleset 13's decisions. [Workloads](workloads.md) owns trial content, placement and coverage; [journal](journal.md) owns event payloads and durable transitions; [runtime](runtime.md#ruleset-13-policy-boundary) owns when the pure policy is called. The decisions are [#1214](https://github.com/shgew/togi/issues/1214) and the owner's answers in [#1215](https://github.com/shgew/togi/issues/1215). Specifications and implementation layers land together; this specification is not a claim that the predecessor binary implements these rules.

## Ruleset

The Ruleset 13 cutover bumps `togi_tuner::RULESET` from its predecessor value; the design number does not require the Rust constant to equal 13. The evidence epoch separately versions workload content, effective configuration, durations and outcome detection. The [fresh transition](journal.md#transitions) archives the current session without tuning carry. Later rulesets must specify their own transition; the raw ledger permits recomputing caps but promises no restoration of D.

The replaced per-core policy is removed, not left selectable: constraints, cores, decide, failures, hunt, later, optimum, phase plans, phases, R7 attribution/backoff, rounds, skip and checking chains/ordinary/reruns. Keep partial-load generation, machine seams, SMU transactions, containment and recovery. Retire the stats tools' chain, counterfactual, R7-attribution and hunt analyses with their callers. Seam-only commits precede the cutover, rather than running two policies.

## Invariants

1. Targets are in [-50, 0]; smaller is deeper. Every core of a CCD receives that CCD's offset. Per-core refinement is outside this ruleset.
2. SMU authorization, teardown and readback obey [runtime's transaction](runtime.md#offset-types-and-the-write-transaction); policy decisions never authorize bypasses.
3. Nights test; days observe without workloads. Only a qualified exact whole profile may become a nonzero day profile.
4. Every decision cites its raw evidence and is reproducible by folding the journal, without querying historical decisions or samples.
5. A day event cannot move U. A night failure cannot silently classify a daily end or clear its veto.
6. Exposure is observed time, never intended time, launch counts converted to hours, sleep, pause, unobserved downtime or an extrapolated crash tail.

## Per-CCD state

For each CCD retain its exposure ledger, mapping cursor/bracket, night edge U, cap S (with calculation witnesses and fallback/measured label), applied day offset D and remaining distance. U has distinct unknown, observed-failure and write-limit-censored states; absence is not offset zero. The whole-profile state holds exact qualifications and their outstanding matrix cells, the ordered day-step history and applied-hour hold, firmware validity, freeze, daily events, profile vetoes, pending questions and reproduction progress/consumed time. Joint trials and daily failures remain whole-profile evidence until an isolated night observation supplies a CCD witness.

A profile is the complete ordered vector of CCD offsets and its expansion to all physical cores. Sparse readbacks, nonuniform offsets within a CCD and missing identity data are observations, not eligible Ruleset 13 profiles.

## Evidence

The exposure ledger is raw observations keyed by `(evidence epoch, firmware context, CCD, trial class, tested offset)`, retaining the full applied profile as provenance and as a joint-class discriminator. Per-CCD aggregation never merges distinct package/interaction profiles. It retains source session/sequence, trial/boot, live or imported origin, outcome, failure signal/location, eligible post-warm-up seconds, pre-warm-up failures and longest uninterrupted eligible duration. A successful trial adds measured eligible seconds; a failure adds only durable measured time and one failure. Early failures still reject qualification and move an eligible mapping edge, but cannot supply positive-time rate estimates alone. An inconclusive trial or failed placement adds no passing exposure. Deduplicate by original identity: copies add no second observation.

The trial-class key is `(regime, workload content identities, effective backend configurations, loaded physical/logical subset, per-CPU roles/instances/phase schedules, light or serial position, intended duration, controller placement/clock contract and credit exclusions)`. Executable bytes, build identity and effective generated configuration identify content; a package store path is provenance only, never the compatibility key. Seeded schedules retain their schedule family and recorded seed; different seeds sample the same declared distribution, not a different class. A changed content identity, effective configuration or epoch is a different class. Unknown old identities never become current exposure by guessing. The [workload spec](workloads.md#evidence-compatibility) owns validity.

Daily observations have a separate ledger of applied hours, faults, ends and answers per exact profile and firmware context. They supply holds and events, not synthetic class exposure. Report observed total crashes and hours beside the CO-0 totals, including provenance and unknown intervals; neither total nor an untested class is claimed bounded.

### Adequate exposure

A class/offset is adequately exposed for coverage once at least one conclusive launch has completed its intended duration, including its internal warm-up boundary, with valid placement and credit. Keep failures and partial exposure before this threshold; they are not passes. Requiring a complete launch prevents many truncated starts hiding delayed failures. [The workload matrix](workloads.md#coverage-matrix) separately owns qualification breadth, sustained cells and tightly scoped unchanged-CCD reuse. This completion threshold does not make every trial purpose eligible for the cap; [the cap's evidence population](#cap-s-margin-from-measured-slope) owns that restriction.

## Cap S: margin from measured slope

The machine's incremental hazard budget is λ* = 1 crash per 3,000 applied hours, divided equally among its n CCDs. This bounds the hazard CO adds over stock, not total crashes. As a conservative upper bound on added hazard, use the class's total observed night failure-rate upper bound without subtracting an uncertain stock estimate; report stock controls separately. This can sacrifice depth but cannot manufacture a negative excess from sparse stock data.

**Adequacy for the cap** requires that completion threshold to be met by this CCD's isolated mapping execution, not by another trial purpose. This includes descent and the [filler cap probes](#filler), whose mapping evidence never completes a descent-board step. The rate projection runs over **mapping classes only**: R1/R2 single-core rotation, R8, R7 with honest partials, and R9 from the [mapping matrix](workloads.md#coverage-matrix). Filter observations by recorded mapping purpose as well as class identity. R3–R6 and qualification, control and reproduction exposure are **uncovered for the cap**, even when adequately exposed for coverage; the status and evidence views must show that distinction explicitly, never assume those classes safe. Short stock controls and shallow qualification cells would otherwise pin S to zero or immediately invalidate a qualified day step. Non-mapping failures retain their separate [stock-control](#co-0-controls), [daily-event and veto](#daily-events), and [isolated-reproduction edge](#night-edge-u) consequences. The broader [slope population](#measured-slope) is deliberately distinct from the projection population.

For each adequately exposed mapping class on a CCD, choose its deepest adequately exposed offset x. For k failures and T eligible hours at x, use the one-sided 95% exact Poisson upper rate `b = chi_square_quantile(0.95, 2(k+1)) / (2T)`; with no failures it is `-ln(0.05)/T`. Count all failures in that cell, including early failures; when T is zero its bound is infinite. The Poisson rate and log-linear projection are modeling assumptions, not demonstrated rare-event guarantees.

### Measured slope

For **every class**, of any purpose (mapping, qualification, reproduction or controls), with failures at two or more distinct offsets and positive exposure, fit `rate(o) = exp(a - beta*o)` by Poisson likelihood to all its eligible offset cells, with exposure as the offset term. Do not constrain beta to the fallback or to be positive. Form a **two-sided 95% profile-likelihood interval** for beta: profile over a and admit values whose twice-log-likelihood loss is at most 3.842. This rounded-up chi-square cutoff is conservative relative to the asymptotic 95% cutoff and makes the numerical rule explicit. Use the lower endpoint, not the point estimate or upper endpoint. An unbounded or nonpositive lower endpoint supplies no justified decay and allows only CO 0.

The CCD's measured beta is the minimum lower endpoint across **every** eligible class, of any purpose, and all earlier measured values in the current evidence/firmware context. After the first measurement, additional non-mapping slope evidence cannot justify a steeper decay or deepen S under the [projection update rule](#projection-and-edge-margin). Before any class has failures at two offsets, use `beta = ln(1.5)`, explicitly labelled **fallback**. The first such measurement replaces, rather than floors at, the fallback; a steeper or flatter first measurement is accepted. A content/epoch/firmware change cannot retain a favourable slope from incompatible facts.

### Projection and edge margin

For a candidate offset s, project each mapping class bound as `b(s) = b(x) * exp(beta*(x-s))`, in both directions from x. The rate-limited offset is the deepest integer in [-50, 0] where every adequately exposed mapping class's projection is at most λ*/n. If no nonzero offset qualifies, use CO 0 as the untuned baseline, not a claim that stock meets λ*. If there is no adequately exposed mapping class, or U is unknown, S is CO 0.

S is the shallower (numerically larger) of that rate-limited offset and `min(U + 5, 0)`. Recompute it after raw evidence changes; journal the binding mapping classes, rate bounds, slope and edge witnesses. After the first measured slope, if an update adds only non-mapping slope evidence, retain the shallower of previous S and recomputed S; a shallower fitted slope must not authorize deeper extrapolation on the deeper side of x. Never round toward greater depth. List each mapping class without adequate cap exposure and every out-of-projection class as **uncovered for the cap**, with the reason; omission is never interpreted as safety. S calculated from the mapping subset remains explicitly coverage-incomplete and alone cannot authorize promotion: the proposed exact profile must fulfill the entire qualification matrix. No annual-rate guarantee is claimed; the wording is **margin from measured slope**.

## Night edge U

U is the shallowest actually observed failing offset in controlled **isolated CCD** mapping/reproduction: the tested CCD is uniform, other CCDs are CO 0, and managed placement is valid. Count computation errors, trial-window MCEs, crashes and [process faults](workloads.md#trial-outcome) on the tested CCD, including togi's controller, dashboard and workers. A fault located outside the tested CCD is visible but not charged to it. An unattributed controlled isolated failure belongs to the tested CCD, not a core. All-CCD failures remain profile-level unless isolated testing reproduces them.

Bracket the first observed failure against the preceding passing mapping step using unit counts; retain the shallowest actual failure as U even when a later pass occurs there. No failure at the write limit produces a **censored edge at -50**, not an invented failure; its numeric boundary may enter the edge-margin calculation with that label. Before reaching that limit without a failure, U remains unknown. A stock failure follows [controls](#co-0-controls), never a negative-offset tuning workaround.

## Scheduling

The night scheduler is a deterministic pure query `NightQuery { now, contents, unavailable }` over folded state. There is no caller-supplied night, end time or available-seconds budget. [Runtime](runtime.md#night-declaration) owns the durable night identity and open/close boundary; the query continues that night or opens one. Clock waiting and trial execution belong to the session.

Let E be the night's **elapsed observed task wall time**: startup, warm-up, execution, teardown and recovery downtime consumed so far, with per-task and per-queue attribution. Only eligible measured execution adds exposure. Preserve these observations across tuning restarts without a closing witness and crash reboots; they do not create a fresh share allowance. Runtime's [signal-stop boundary](runtime.md#night-declaration), unlike a crash, closes the night even when tuning restarts in the same boot. Operator-stopped idle time is not task consumption. The planner chooses a cell without asking whether it fits any remaining time. An operator end mid-cell interrupts the trial under the existing containment and outcome rules; unmet cells remain due, never become passes.

### Night order and reserved budgets

Visit work in this order:

1. **Reproduce** the oldest CPU-suspect daily event with an open reproduction task, exact failed profile first, then each CCD alone in ascending CCD-ID order. Use the qualification matrix's classes in their deterministic order, stopping a profile's attempt at its first conclusive failure. One event receives one night only; a crash reboot into tuning continues that same night. Record remaining cells and mark an uncompleted or all-clean attempt **unreproduced** when that night closes under [runtime's boundary](runtime.md#night-declaration). A nonuniform failed profile, whether imported or manually recorded, cannot be a night target: at its first reproduction opportunity in an open night, close the task as **untestable**, with no target and no trial. Do not approximate it by a uniform CCD vector; its veto and event resolution still follow [the daily-event rules](#whole-profile-veto-and-escalation). Clean trials do not clear the event or veto and earn no qualification credit. Isolated failures may move U; an exact-profile-only failure stays profile-level.
2. **Qualify** the next admissible exact day profile through the [composite coverage matrix](workloads.md#coverage-matrix). That section alone owns same-offset unchanged-CCD reuse; fresh package cells always use the complete proposed profile, and no componentwise-deeper pass substitutes.
3. **Map** CCDs in ascending ID, taking one mapping offset per CCD before returning to the first. Descend from 0 by five counts, rotating every R1/R2 core position and catalog, R8 light positions, R7 full/partials and R9 serial positions. At each offset complete the mapping matrix before descending, or stop that step at its first failure. Bracket the first failure in one-count steps from the last passing coarse step, then sweep each integer from U toward S on later nights. If S changes, retain raw facts and schedule the still-uncovered shallower sweep cells.
4. **Controls and re-qualification** as below.
5. **Fill** otherwise idle work with the [filler task](#filler); an empty primary queue is not an end-of-night condition.

The former reservations are **soft shares of E**, not fractions of a predicted night: mapping 10%, due stock controls 5%, due monthly re-qualification 10%, reproduction 25% and next-step qualification 75%. A due queue at or over its share yields only while another due queue is under its own share. Choose the first under-share due queue in the priority order above when one exists; otherwise choose the first due queue. At E = 0 none is below its share, so ordinary priority chooses the first task. Work may cross a share in one cell, and an uncontested queue can run beyond it; shares are neither cell-fit tests nor simultaneous guaranteed allocations. First-night/changed-firmware stock controls remain a prerequisite: nonzero queues are inadmissible until they complete, regardless of shares.

D re-qualification is due 30 elapsed calendar days after its last complete qualification, or immediately when that identity is invalidated. Thirty days is a conservative fixed monthly interval that avoids calendar-month ambiguity. Re-qualify the exact current D using the full matrix; qualification may span nights. Expiry blocks promotions, not observation at D. Any conclusive failure becomes a CPU-suspect daily event at D through the same event rules.

### Filler

Filler is its own night task kind, selected only when no runnable mapping, qualification, reproduction or due control/re-qualification cell remains. It first probes **one count deeper than S** on the admissible CCD furthest from its limit: maximize `S-U`, breaking ties by ascending CCD ID, and rotate the mapping matrix's cells by a durable filler ordinal. An unknown U permits no cap probe. In depth magnitudes the admissibility rule is `depth(candidate) <= depth(U) - EDGE_MARGIN`, where `depth(o) = -o` and `EDGE_MARGIN = 5`; equivalently for signed CO, `candidate >= U + 5`. This notation keeps the owner's edge-margin condition consistent with the signed offsets used here; a probe must not bypass the existing edge margin or write limit.

A filler cap probe uses the isolated mapping contract and records mapping activity/exposure, including eligible U and slope evidence. It **never finishes a MappingBoard step** or supplies an exact-profile qualification. If no cap probe is admissible, re-run D's exact qualification cells round-robin with a durable cursor, retaining the normal exact-profile contract; a failure follows the [current-D re-qualification event path](#whole-profile-veto-and-escalation). Skip unavailable-backend cells without marking them complete; they remain uncovered/outstanding. The two filler forms retain their underlying mapping or qualification trial activity while `night.plan` records the distinct filler task kind.

### Night query results

`next()` returns a runnable task whenever any cell is runnable, including filler; there is no deadline, completed-work or insufficient-budget `EndNight`. Its policy `EndReason` is exactly `DeadEnd` (confirmed stock instability) or `Unavailable` (**no runnable cell remains without an excluded backend**). The latter includes every configured backend being exhausted, or required first-night/firmware stock controls still needing an excluded backend with nothing else admissible. The session separately enforces [runtime safety stops](runtime.md#run-recovery), including a thermal trip (exit 18), an unresolved too-aggressive defect (exit 17), and inherited dead ends. A stuck backend makes only that backend unavailable for the rest of the night under runtime's retry/exhaustion rule; another usable backend prevents an end only while it has an admissible cell to run.


## CO-0 controls

On the first night and after every firmware-context change, run the entire coverage matrix at all-zero before a nonzero test. A conclusive failure at all-zero is already confirmation at stock: stop with `platform unstable at stock under <class>`, naming the class and evidence. Do not keep tuning around it, attribute it to a nonzero CCD, or claim CO caused it. A failure at a nonzero profile that names a CO-0 core instead requires one conclusive rerun of the same class and duration with **every** CCD at CO 0; a passing rerun is not proof of causation and the original failure remains profile-level unless isolated testing locates it. Inconclusive controls give no clearance. A new run cannot ignore an unresolved stock-failure stop; it needs an explicit reset and fresh controls.

## Day steps

The pure policy returns hold, promote, retreat or firmware-zero with its reason and witnesses; it never writes hardware. [Runtime](runtime.md#ruleset-13-policy-boundary) owns invocation and successful-application commits.

### Hold and next proposal

Begin at all-zero D. The first nonzero step needs no hold. Every later promotion needs **50 applied daily hours at the current exact D**, after its successful application and the latest CPU-suspect event at it. Count only checkpointed intervals with verified unchanged readbacks and active observation. Suspend, pause, unknown/changed-profile intervals, service downtime and tuning hours are excluded. A non-CPU fault does not reset the hold. A profile change starts a new hold; re-visiting a historical profile does not reuse its old hours. If a provisional event is reclassified, recompute the hold from the raw intervals rather than losing valid time permanently.

For each CCD with `D > S`, remaining distance is `D-S`. Choose the largest distance, breaking ties by ascending CCD ID. Move only that CCD by `max(1, ceil((D-S)/2))` counts toward S. Rounding up makes every step progress and avoids an extra almost-half step. Stop exactly at S, never past it. Other CCDs keep D. Recompute the proposal when caps or vetoes change; an old qualification of another vector cannot qualify it. If the chosen proposal is vetoed, hold and report its blocking veto; do not silently change CCD or invent a different step.

### Eligibility and constraints

Apply a promotion only when its exact profile is night-qualified under current content, epoch and firmware; the hold is complete; no question is pending; no daily event is unresolved; freeze is off; and the cold boot did not start paused. The profile is never deeper than any CCD's current S and must satisfy every active veto. A clean reproduction is not qualification. Qualification does not clear an unresolved event. A cap becoming shallower than D schedules a safety retreat to the shallower componentwise cap, without a hold or promotion; if the resulting exact profile is not qualified, use CO 0 until it is. CO 0 needs no claim of qualification to be the fallback.

Freeze retains the current D and prevents promotions, never blocks a required retreat or firmware reset. Paused time cannot fulfill a hold or count as applied exposure. A cold boot that starts paused never promotes, even if the pause ends later in that boot; required step-backs and vetoes still take effect in the policy decision at that boot and apply when the pause ends (owner, 2026-10-24). Pause command forms, duration and service lifecycle belong to the applying layer and are not decided here.

### Firmware

A BIOS, AGESA or microcode change invalidates active qualifications, slopes and caps. Set D to CO 0 at the next application opportunity; retain the previous profile as a re-qualification target and old raw observations as mapping hints only. Complete new-context stock controls and an exact-profile night qualification before restoring a nonzero profile, also satisfying the recomputed cap and vetoes. Old facts never contribute current hours, rate or qualification. This reset is not undone by a same-boot restart.

## Daily events

### Classification and pending questions

Gather evidence and ask after an unclean end; an orderly shutdown or reboot, including a normally handled power button, is never a crash. [Runtime's evidence gathering](runtime.md#day-observation) and [journal day observations](journal.md#day-observation) own the raw format and answers. Classify a core-local or uncorrected MCE as CPU-suspect even in a cleanly ended boot. A `frozen` answer is CPU-suspect; `graphics` or `power_off` reclassifies an otherwise unexplained end as non-CPU; `unsure` supplies no definitive cause. An answer cannot erase independent core-local/uncorrected MCE evidence. Other faults remain visible and reset the hold only if classified CPU-suspect. A CO-0 event is platform/background evidence, not a reason to tune deeper or charge an arbitrary CCD.

At nonzero D, an `unsure` answer, or an Unknown or CF9 end left unanswered, becomes a **provisional CPU-suspect event**: create its veto, require retreat of the newest step at the next cold boot, and assign one reproduction night under [escalation](#whole-profile-veto-and-escalation) (owner, 2026-10-24, confirming the 2026-10-20 rule). Its question remains answerable but stops counting as pending once the provisional event forms. Other pending questions still block promotion even if a reset or an unrelated answer changed other state. Its veto and step-back persist after resolution; a later reclassifying answer undoes them. Recompute from durable step history, respecting all other events, caps and qualifications, rather than blindly restoring an unsafe offset.

### Live faults and load mix

Consume `day.fault` at its recorded profile under [the classification rules](#classification-and-pending-questions), including faults in cleanly ended boots (#1215 Q15). A machine check with a known core is core-local; an uncorrected machine check is CPU-suspect even without a known core. A corrected machine check without a known core, kernel errors and process traps remain raw visible daily evidence, with no automatic retreat, veto or hold reset unless other evidence classifies the event CPU-suspect. No new CPU-causation rule for those faults is inferred from their presence. Use the journal's fault identity to deduplicate observations and imports of the same fault.

`day.checkpoint.load_mix` describes sampled daily load shapes, as decided in #1214. Preserve and show it beside applied-hour and fault evidence, but never turn its shape milliseconds into trial-class exposure, qualification, a cap, or an additional hold/promotion condition. The [runtime](runtime.md#load-mix) owns sampling and shape definitions; checkpoint active time and eligible hold intervals remain different quantities.

### Whole-profile veto and escalation

A CPU-suspect daily event, including a manually recorded crash, vetoes the **whole applied profile** P: no later **day** profile Q may satisfy `Q[c] <= P[c]` for every CCD c. Preserve the actual failed profile and its provenance; a nonuniform historical profile is compared on all its physical cores, never averaged into a fictitious uniform CCD profile. Vetoes stand until evidence reclassifies their source event, never expire, and are not cleared by clean nights, a reset of tuning evidence, or elapsed holds. Controlled-night reproduction is the explicit exception to the day-use ban, within its one-night budget; it never earns qualification credit.

At the next cold boot revert the newest applied promotion, restoring its preceding whole profile. Keep an escalation record tied to this event and stepped-back profile, **without expiry**, with a durable cursor into each CCD's successful day-step history. Reverting a step consumes it in this chain: the first retreat consumes the newest promotion on its changed CCD. A further CPU-suspect daily event at the stepped-back profile, however much later, makes **every** CCD revert its own newest remaining step to its previous day value; a CCD with no remaining nonzero step stays at CO 0 (owner, 2026-10-24). Advance each cursor past the reverted step, so a third event at the resulting profile consumes the next remaining step on each CCD rather than repeating an already consumed retreat. For example, with CCD0 history `0 → -8 → -12 → -14` and CCD1 `0 → -10 → -15`, if CCD0's `-14` was newest overall, the first event restores `(-12, -15)`, the second restores `(-8, -10)`, and the third restores `(0, 0)`. If a needed retreat profile lacks qualification, apply CO 0 until the retreat profile qualifies; never deepen as part of a retreat. At CO 0 report platform/background cause. Multiple observations of one end are one event, not multiple escalation strikes. A revised classification recomputes the chain from raw events and step history.

A night's failed monthly re-qualification enters this same whole-profile event path. Reproduction closes its bounded task with a reproduced, unreproduced or untestable result under [the scheduler](#night-order-and-reserved-budgets), not the event's veto. A CPU-suspect event, provisional or definite, is resolved for promotion only after its required retreat is successfully applied and its reproduction task is closed; journal those witnesses while leaving its veto and escalation active. This includes an untestable provisional event: closing its task does not bypass the required applied retreat. This conservative resolution boundary prevents an immediately acknowledged crash from bypassing its outstanding safety work without blocking promotion forever on an unanswerable question. A non-CPU reclassification resolves the event and removes only its own provisional effects. No clean reproduction supplies a classification.

### Manual crash records

Every [`day.record`](journal.md#ruleset-13-events) is a CPU-suspect manual event and a whole-profile veto at its recorded offsets, under [the event rules](#whole-profile-veto-and-escalation). Its crash identity is the named crashed boot for `UncleanEnd { boot }`, otherwise the crash instant for `Unobserved { at }`. Deduplicate a record against that same boot's unclean end and copies in transition imports: these are witnesses of one event, not additional escalation strikes. Keep its entered/read-back offset provenance, provenance timestamp, running category and optional note without treating them as measured trial exposure.


## Session start

Require a verified all-zero starting baseline and record topology, firmware context and workload content. Nonzero or incomplete baseline readback is a refusal, not clamping or advice. The first night establishes controls and mapping; the first day proposal follows the pure rules above. The [journal transition](journal.md#transitions) owns imports of pre-13 daily observations, including stock hours and the recorded freezes.

## Dead ends

The existing hardware, containment, preflight, thermal and unresolved-defect stops remain runtime safety policy; backend exhaustion is narrowed under [night query results](#night-query-results). A stock failure uses [CO-0 controls](#co-0-controls). No per-core failure point, combination, hunt fallback or held-later-failure rule survives to reinterpret it. Journal failure follows the zero-only emergency contract, never an invented successful observation. Replay must stop before another write when durable evidence already establishes a stop, including a kill before its `deadend` event.

## Historical rule anchors

The following anchors preserve links from historical ADRs and predecessor-tool documentation. They do not retain policy, aliases or compatibility code. [ADR 0080](../adr/0080-per-ccd-first.md) replaces the per-core rules; [ADR 0079](../adr/0079-day-observer-night-tester.md) replaces cycles/phases with the night/day policy. Historical facts stay in archived journals.

### Search

Replaced by [night mapping](#night-order-and-reserved-budgets).

### R7 request order and attribution

Request telemetry now selects [partial loads](workloads.md#regimes), never blame.

### R7 self-sufficiency evidence

Replaced by [raw class exposure](#evidence), not a per-core safety label.

### R7 voltage-targeted backoff

Removed; [cap S](#cap-s-margin-from-measured-slope) owns margin.

### Testing a repeatedly charged requester

Removed with attribution; [coverage](workloads.md#coverage-matrix) includes partials directly.

### Escalation to a located hunt

Removed; [daily-event escalation](#whole-profile-veto-and-escalation) is a different rule.

### Failure points and combinations

Removed; [whole-profile vetoes](#whole-profile-veto-and-escalation) are daily evidence, not inferred culprit sets.

### Trial alone sequence

Replaced by managed isolated-CCD mapping and the existing [write transaction](runtime.md#offset-types-and-the-write-transaction).

### Together trial sequence

Exact qualification uses [coverage](workloads.md#coverage-matrix) and the existing [runner containment](workloads.md#containment).

### Crashes

Night failures follow [U](#night-edge-u); daily ends follow [daily events](#daily-events).

### Decision events

See [the new event contract](journal.md#ruleset-13-events).

### Checking

Removed; [exact-profile qualification](workloads.md#coverage-matrix) replaces reusable cycle passes.

### Hunt

Removed; bounded [night reproduction](#night-order-and-reserved-budgets) is not group-testing attribution.

### Phase 1

Removed; [days begin at zero](#day-steps).

### Phase 2

Removed; [day steps](#day-steps) have no solo-limit refinement rounds.

### Later failures

Removed; [daily events](#daily-events) have no cycle-expiry window.

### BIOS profile

No export or automatic BIOS recommendation; [ADR 0082](../adr/0082-bios-stays-at-zero.md) keeps BIOS at CO 0.

### Defect list

Historical defect provenance stays visible; no old decision or per-core reset can seed this policy. See [transition](journal.md#transitions).

### Reset

Retired; see [reset commands](journal.md#reset-commands) and [transitions](journal.md#transitions).
