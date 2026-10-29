# Ruleset 14 (specified; not yet implemented)

This specification records the approved two-ends D decision in [#1458](https://github.com/shgew/togi/issues/1458), tracked by [Ruleset 14 #1460](https://github.com/shgew/togi/issues/1460). **The binary does not implement these rules. [Ruleset 13](tuner.md) remains the normative description of shipped behavior.** This sibling keeps future policy separate from the current executable contract; it is not a selectable alternative. No compatibility constant, schema number, evidence epoch or deployment authorization changes here.

Within this document, “must”, “retains” and present-tense policy descriptions specify the successor implementation contract, not a claim about the shipped binary. Sections 1–11 preserve the design’s numbering. [Workloads](workloads.md), [journal](journal.md) and [runtime](runtime.md) continue to own their respective execution, durability and application contracts; the disposition section identifies the policy changes rather than replacing those contracts.

## 1. Scope, compatibility and terminology

Rules below apply to a **tuning unit**: a CCD in the coarse phase; a physical core in later refinement. The whole profile is always the complete ordered per-core vector plus its topology and phase/unit mapping. A CCD offset expands uniformly to its member cores; a core offset expands to that physical core's SMT siblings. No daily crash identifies a guilty unit.

D needs a **new ruleset**, not an unversioned fix to Ruleset 13. Separate authorization reserves the RULESET bump and its number; the Ruleset 13 design number is already distinct from `togi_tuner::RULESET = 3`. Any required SCHEMA or evidence-epoch decision also requires separate authorization. This design assigns none of those numbers.

The settled policy specifies the two-ended climb, initial and subsequent 50-hour holds, bounded rounded-up steps, exact-profile qualification, newest-step stop, whole-profile veto/escalation, firmware-zero/one-step restoration, two-event machine allowance and honest evidence wording. The decisions below are settled; implementation and hardware acceptance remain outstanding.

The eventual CO-added-hazard goal remains `λ* = 1 crash per 3,000 machine-hours`; bounded exploration is not permission to raise that goal or claim it has already been met.

## 2. Night edge and hard ceiling

Use signed offsets in [−50, 0], with smaller values deeper. For each unit i:

- `U_i = unknown` until a controlled isolated mapping/reproduction failure is observed or the complete mapping descent reaches the write limit without failure.
- `observed(u)` means the shallowest actually failing isolated offset in the current firmware/evidence context. Later passes never erase that failure. The tested unit alone is nonzero; all other units are at CO 0. Placement, outcome precedence, process-fault handling and stock confirmation retain their existing contracts. A joint failure never becomes an isolated edge by guessing its culprit.
- `censored(-50)` means complete required mapping at the write limit without a failure, not an invented failure at −50. Before completing that descent, U stays unknown. Qualification alone cannot manufacture a censored edge.
- Retain the fixed five-count edge margin for both observed and censored edges: `C_i = min(u + 5, 0)`. Thus observed U = −30 permits C = −25, observed U = −4 permits only stock, and censored U = −50 permits C = −45. Unknown U gives no ordinary nonzero promotion permission for that unit. C is the hard night ceiling, separate from statistical S; the narrowly specified firmware-restoration exception is in §7.

A proposal may not be deeper than any applicable C. A newly observed shallower edge is genuinely adverse evidence: if it makes `D_i < C_i`, retreat to a componentwise shallower permitted profile without waiting for a hold. Apply that target only if its exact qualification is valid; otherwise use CO 0 until it qualifies. A unit whose applied step is undone by this adverse retreat stops climbing under §5. Unknown or incompatible evidence after firmware change takes the reset path, not an assumed edge of zero.

The U+5 choice is a margin, not proof about daily hazard. The successor gate must show the physical −50 frontier and the operational −45 ceiling separately on censored write-limit cases. Neither design changes that margin after seeing scores.

## 3. What exactly qualifies the next profile

A candidate P is qualified only by a complete current-context **composite** certificate for that exact full vector. Freeze the matrix, workload/configuration identities, placement contracts, intended durations, firmware, evidence epoch and rotation ordinal when qualification begins. The certificate lists original observation identities, fresh whole-profile cells, permitted same-offset reuse, outstanding cells and conclusive failures. An interrupted or inconclusive cell remains outstanding; a clean reproduction is never qualification. An unavailable backend supplies no missing credit.

The unchanged [coverage matrix](workloads.md#coverage-matrix) owns every required class, breadth, duration, placement and allowed reuse; no shorter successor matrix is introduced.

The workload spec continues to own catalogs, exact subset construction, SMT-position rotation, controller exclusion/repeats and class identity. A core-phase step **does not shrink** the changed CCD's R1–R5 requirements merely because one core moved; per-core savings must come from starting closer and legal reuse, not dropping coverage. R7's physical layouts remain topology-defined, not sixteen fabricated independent power domains.

Use only the matrix’s same-offset unchanged-CCD local reuse, with its complete compatibility and intervening-failure conditions. Package/interaction cells are fresh at the exact candidate; deeper-vector passes and additive component certificates do not certify it. First controls, changed-firmware invalidation, monthly full requalification and warm-up/exposure exclusions remain owned by [workloads](workloads.md) and [Ruleset 13 scheduling](tuner.md#scheduling).

For two eight-core CCDs and up to twelve partials, a normal changed-CCD certificate schedules 155 launches / 19,380 s ≈ 5.38 h, before hardware overhead, placement repeats and interruptions. **[INFERENCE: matrix arithmetic, not a measured real-night timing promise.]** Cells span nights rather than being shortened to fit.

A conclusive failure of a proposed, unapplied P durably blocks that exact vector in the current session/firmware context, including a kill before the derived failed-certificate event. Retain D; do not evade the block by choosing another unit or a smaller ad hoc step. A genuinely changed C can produce a different deterministic proposal. Failed candidate qualification consumes neither a daily step nor the daily learning allowance. A failure re-qualifying the **current** D enters the existing CPU-suspect event path instead.

## 4. A clean hold earns one step

At each eligible cold boot, perform safety decisions first. A restart, resume or pause expiry can re-apply a permitted result, never promote. For ordinary promotion:

1. Start from verified all-zero in a fresh D session, then observe the **current exact applied profile** for 50 clean daily hours. The rule is that the initial stock profile must earn these hours too; remove Ruleset 13's first-step exemption. Stock controls and candidate qualification can run while this hold accumulates.
2. Count only checkpointed observed intervals with verified unchanged readbacks and active observation after successful application and after the latest applicable CPU-suspect event. Exclude sleep, pause, tuning, unknown-profile intervals and service downtime. A profile change starts a fresh hold, even if that vector was used before. Non-CPU faults do not reset it; recompute valid intervals after an answer changes classification.
3. Among unfinished units with `D_i > C_i`, choose maximum distance `r_i = D_i − C_i`, ties by ascending topology unit ID. One unit moves. Retain upward integer rounding of the half distance: `step_i = min(5, max(1, ceil(r_i / 2)))`; set `P_i = D_i − step_i`, other units unchanged. For r = 0 there is no step. Since r is a positive integer, this never passes C.
4. Apply P only after the 50-hour hold, its exact qualification, no pending question, no unresolved event/safety work, no active veto violation, valid firmware, freeze off, available machine allowance, the nonzero hardware activation gate, and the existing safe-apply prerequisites all hold. A boot starting paused cannot promote later in that boot.
5. Commit the step/history/hold reset only after successful full-profile application and readback. A planned or failed application is not an earned or consumed step. Recompute P if C, vetoes or context change; an old certificate for another vector cannot qualify it.

Example with C = −25: `0 → −5 → −10 → −15 → −20 → −22 → −24 → −25`. Seven promotions per unit, fourteen for two units, with fourteen preceding 50-hour holds: approximately **700 applied daily hours**, or 87.5 eight-hour days. **[INFERENCE: no faults, vetoes, mapping delay, qualification delay, firmware changes or lost observation.]** Completion is not waiting for a new crash and does not require a crash to discover a result.

## 5. Newest-step blame, stop and the machine allowance

Retain the existing classification: an orderly shutdown is not a crash; core-local or uncorrected MCEs remain CPU-suspect even in cleanly ended boots; an otherwise unexplained `unsure` answer or unanswered Unknown/CF9 end at nonzero D forms a **provisional CPU-suspect event**. Non-CPU answers cannot erase independent MCE evidence. Manual records retain the actual profile and provenance; copied witnesses never create extra events.

A CPU-suspect event vetoes its **whole applied profile** P and every day profile Q at least as deep on every physical core. Vetoes do not expire, and a clean night cannot clear one. Only source-event reclassification can clear that event's veto. Controlled reproduction retains its explicit one-night exception and earns no qualification credit.

At the next cold boot:

- Revert the newest successfully applied promotion to its preceding profile. The changed unit **stops climbing permanently**; its last clean depth is its result, not a new invitation to explore from there. Newest-step blame is a conservative stopping heuristic, not causal localization. A successful hold cannot exonerate an older unit.
- Each further CPU-suspect event at the stepped-back profile, without expiry, makes **every tuning unit** revert its own newest remaining step to its previous day value. Durable per-unit history cursors consume each reverted step; subsequent events continue toward CO 0 rather than replaying the same retreat. A unit with no nonzero history stays at stock. Every unit that takes its first such backstep also stops climbing. Emit a notice on every retreat, naming profile, reason, provisional/definite status and allowance state.
- The first event's safety work is resolved only after its required retreat is successfully applied and its one reproduction night closes (reproduced, unreproduced or untestable). That resolution does not restart a stopped unit or remove its veto/escalation history. Pending unrelated questions still block promotion.

Stopping each unit on its first step back gives approximately one learning event per unit: **two on a two-unit coarse phase**, rather than repeated probes after a failed daily trial. It is an allowance for risk, not a promise that there will be at most two crashes: events can occur during an already-running profile, after finish, during requalification or while waiting for safe application.

**Machine-wide enforcement and core scaling:** the campaign has a single durable **machine-wide allowance of two unique CPU-suspect learning events** for a two-CCD/16-core campaign, shared across the coarse and core phases. This is stricter than automatically granting sixteen new events on entering core refinement. Provisional events and current-D requalification failures consume it; event identity is deduplicated across boots, records, imports and source witnesses. At exhaustion, stop all additional climbing; mandatory retreats, vetoes, zeroing, controls and observation remain active. Do not hide a third event or stop counting after exhaustion.

The allowance is not an annual hazard budget, and unused allowance is not permission to retry a stopped unit. Track learning events while any unit is still climbing separately from post-finish events. Every unique CPU-suspect event at a nonzero campaign profile consumes the same machine ledger, including post-finish events before core refinement; phase labels never hide an allowance charge. CO-0/background-classified events do not consume it. All events remain visible and all safety rules still apply. No automatic service restart, reset, firmware update, phase change or ruleset transition replenishes this campaign allowance. A new allowance requires an explicitly approved new campaign; a missing/unreconstructible allowance is not assumed unused.

**Later reclassification:** retain the existing guarded restoration of a promotion undone solely by an event later established non-CPU, subject to a fresh normal hold, exact certificate, ceilings, vetoes and other blockers. It may restore the contradicted depth, but does **not** reopen that unit's climb or refund the campaign's historical allowance. Show the raw consumed count and subsequently reclassified count separately. This resolves the tension between provisional events counting and Ruleset 13's restoration path without turning reclassification into a new experiment.

## 6. Finish, steady use and the words shown

A unit finishes when its applied D reaches C or its first applied backstep stops it. A blocked proposed qualification, an unknown edge or an unfinished hold is **held/unfinished**, not a successful finish. An exhausted machine allowance ends exploration with reason `allowance exhausted`; do not relabel that as reaching the night ceiling. Record per-unit finish reason, time, profile, edge/ceiling witnesses and last clean depth. The rule is that ceiling completion is sticky within the campaign; later deeper night findings do not silently restart it. Adverse findings still retreat it.

The resulting profile remains in normal use, collecting actual-profile hours. Monthly full requalification, faults/questions, whole-profile vetoes, escalation and firmware resets continue after finish. Finish means the exploration stopped, not that the machine became reliable or that future events are waived.

Status and the evidence view must distinguish:

- applied/read-back profile from proposed or paused saved D;
- D, C, tagged U, current qualification and uncovered classes per unit;
- hold hours, phase, finish/hold reason, latest step and its history witnesses;
- `permission: experimental hold + exact night qualification`, not `S certifies this step`;
- machine learning allowance consumed/remaining, provisional count, subsequently reclassified count, and every post-finish event;
- observed total daily crashes/hours beside CO-0 totals and load mix, with unknown intervals, provenance and firmware context.

Use **`experimental: N clean hours; at most 1 crash per X h (95%, fixed-rate model)`**, for example `experimental: 50 clean hours; at most 1 crash per 17 h (95%, fixed-rate model)`, not “stable” or “meets your goal.” At zero failures, the pointwise one-sided 95% Poisson upper rate is `−ln(0.05)/T`; the displayed reciprocal X is `T/2.9957`, with T in compatible current-profile observed hours. Approximately **8,988 crash-free hours**, conventionally rounded to **9,000**, are needed to support 1/3,000 h under that model: about **3.1 years at eight observed hours/day**. **[INFERENCE: model arithmetic; neither a hardware prediction nor a simultaneous lifelong/adaptive confidence guarantee.]** Fifty hours are a permission hold, about a 1/17 h upper bound, not rare-event proof.

“Meets your goal” is allowed only when the **current exact profile and compatible context's daily evidence** supports λ* under the explicitly stated statistical assumptions, never because the climb finished. Do not pool hours from earlier shallower profiles, qualification or CO 0 into that profile's claim. With nonzero failures use the corresponding exact Poisson upper bound, not `3/T`. The pointwise model qualification must remain visible; repeated/adaptive looks, workload drift, missed crashes and stock uncertainty preclude an unconditional safety certificate. Conservatively bounding total observed profile crashes without subtracting uncertain stock hazard can support an upper bound on added hazard; it does not establish that CO caused every counted event. Finishing with unresolved questions or observation gaps cannot manufacture such a verdict.

## 7. Reconcile every current decision path

| Current `tuner.md` rule | D disposition and testable consequence |
|---|---|
| Statistical cap S gates promotion and chooses its destination | **Replace.** C and the hold/qualification/allowance contract govern promotion. S may remain an explicitly non-authorizing diagnostic projection for #1221, with its assumptions, witnesses and gaps; no code path may consult low S as an implicit permission block. Do not merge B′. |
| A shallower S forces retreat | **Replace.** Low S, sparse exposure, a changed fitted slope or missing projected coverage alone cannot undo an otherwise permitted D step. Actual shallower U/C, current-D qualification failure, active veto, invalid context and runtime safety stops still retreat/zero through their explicit paths. No generic “night data changed” reason may secretly restore the S constraint. |
| Night mapping, reproduction, soft shares, controls, monthly requalification | **Keep** [the scheduling contract](tuner.md#scheduling): deterministic operator-ended nights, measured-time accounting, queue shares, bounded reproduction, first/changed-firmware all-zero controls and 30-day requalification. **Replace** S-directed shallower sweep/filler intent: map/bracket U, sweep uncovered cells from U toward C; when primary work is empty, repeat exact-current-D qualification cells with the durable cursor. Remove S−1 filler; it cannot unlock permission. |
| Failed next-profile certificate | **Keep** the exact-vector block specified in §3. |
| Whole-profile veto and escalating retreat | **Keep and extend** through §5's unit-generic history, permanent stops and machine allowance. |
| First step exempt from hold; half distance to S | **Replace** with §4's initial hold, bounded half-distance-to-C rule and deterministic unit order. |
| Freeze | **Keep** D and stop promotion only. Required retreat/reset still wins. Freeze is not pause-to-stock. |
| Firmware change | **Keep zeroing and incompatibility; retain #1214's restoration direction.** BIOS/AGESA/microcode change resets the applied profile to stock and invalidates active qualifications, slopes and holds. New-context stock controls and night qualification of the **exact previous full profile**, followed by **50 clean observed stock hours**, permit restoring that profile in **one day step**, not an ordinary multi-step re-climb. This is an explicit exception to the ordinary one-unit/step-size rule. Old positive facts supply no current qualification/hours; genuinely adverse current-context evidence and active vetoes still block restoration. Preserve allowance, permanent unit stops and prior history. Restoring previously stopped units' saved permission does not reopen their climb. |
| Pause (#1235) | **Keep all forms, lifetime and notices.** Zero every core and read back; preserve learning; paused hours never count. A paused cold boot cannot promote, and safety decisions apply to the saved D before resume. Resume/expiry re-applies only a still-permitted profile through the full transaction. A crash actually at paused CO 0 is platform/background evidence, not a charge against a saved nonzero step. |
| Watchdog/apply (#1218) | **Keep** [runtime's safe-apply contract](runtime.md#offset-types-and-the-write-transaction) and its [policy boundary](runtime.md#ruleset-13-policy-boundary): verified stock baseline, PID1's armed/counting 60 s hardware watchdog, no lockup panics by day, defect/topology refusals, full transaction, emergency zeroing and no-promotion restart/resume. D changes the tuner's risk policy, not the applier's authority. |
| Raw exposure, warm-up, stock failure and runtime dead ends | **Keep** [workload outcome/exposure and containment rules](workloads.md) and [runtime safety rules](runtime.md); daily holds in §4 never become synthetic class exposure or permission to tune around stock instability. |
| BIOS zero and session transitions | **Keep** BIOS CO 0, no export or automatic BIOS recommendation, archive history honestly; §10 specifies the successor transition separately. |

A unit not yet stepped into a profile cannot consume a fictitious “previous step.” If a safety retreat target has no valid qualification, stock is the fallback, not a claim that the shallower target passed. A required safety backstep has priority over restoration, promotion, freeze or allowance availability.

**Firmware restoration history:** save the exact pre-change profile and its unit mapping, reset to stock, and record the subsequently successful restoration as one whole-profile **learning step** from stock. The stock hold and the exact new-context night certificate are both mandatory; do not substitute a componentwise-deeper pass or old firmware hours. Keep ordinary pending-event, veto, remaining-allowance, freeze/pause, watchdog, safe-apply and activation checks. A genuinely adverse current-context ceiling finding takes precedence; an old U is not current positive evidence, and absence of a newly mapped U alone does not turn this explicit restoration path into a mandatory re-climb.

If that restoration has a CPU-suspect event, including a provisional event, consume the same machine allowance, veto the full restored vector and step the restoration back to stock. Mark the restored tuning units stopped, preserving existing stops; whole-profile restoration supplies no isolated culprit. Further events retain the ordinary unexpired escalation path. Report firmware-zero time, requalification time, stock-hold time and restoration outcome separately, with no safety-grace exclusion in the gate. This exception is only restoration of the exact previous profile, never permission for new depth or reopening a completed/stopped unit.

## 8. Per-core refinement is the end goal, not a restart from zero

CCD tuning is coarse preparation. The subsequent core phase uses the **same state machine and rules**, not the removed hunts/requester-blame policy. Implementing that phase remains a separately planned merge unit under [#1222](https://github.com/shgew/togi/issues/1222); this design fixes its interface and cost contract now so D cannot bake “two CCDs” into history, allowance, events, scheduling, renderers or benchmark dimensions.

1. Enter refinement from the last clean, qualified coarse profile: each physical core inherits its CCD's D, plus the real ancestry of the successful coarse steps. Do not replay a new 0-to-D climb sixteen times or duplicate a parent application into sixteen fictitious promotions. Permanent stopped state and machine allowance carry forward. The rule is that a unit stopped for adverse evidence is not reopened merely by subdividing it; its cores cannot use phase change to evade the stop.
2. Nights search each eligible core's isolated edge U with all other cores at stock, including idle/SMT and relevant neighbour/partial interaction coverage. A CCD edge is a mapping hint, **not a per-core U certificate**. Exact whole-profile qualification is still needed after every proposed one-core step, with fresh package cells; independent core passes cannot certify their joint vector. If core-specific evidence makes its ceiling shallower than inherited D, use the adverse-evidence retreat, not a deeper trial.
3. Days confirm a single core's bounded next step from that inherited depth. Pick the largest remaining distance, ties by core ID, and wait another 50 exact-profile hours for every change. Night work finds the edges and prepares the next certificate while day holds accumulate. No batching sixteen changes into one profile update, no crediting one hold sixteen times, and no parallel daily experiments to hide cost.
4. Preserve a durable link from inherited per-core history to each actual coarse application. Second-event escalation consumes each core's newest remaining actual depth transition, including shared parent transitions, and applies one consistent whole-profile retreat. Replay must reproduce that without multiplying events or allowance charges.

**Daily-hour planning, not measured convergence:**

| Case | Promotions per eligible unit | Applied daily hours, before waits |
|---|---:|---:|
| Coarse phase: two units, 0 to C = −25 | 7 | 700 h |
| Refinement: sixteen cores, inherited result already at C | 0 | 0 promotion-holds; observation/requalification still continue |
| Refinement: each core has 1 additional count to C | 1 | 800 h |
| Refinement: each core has 2 additional counts to C | 2 | 1,600 h |
| Refinement: each core has 5 additional counts to C | 3 (3, 1, 1 counts) | 2,400 h |
| Rejected restart-from-zero example: sixteen cores, 0 to −25 | 7 | 5,600 h |

At eight applied hours/day the 700 h coarse example is 87.5 days; adding one or two core steps each makes approximately **1,500–2,300 h total**, or **188–288 active-use days**. Five additional counts per core makes 3,100 h total, approximately 388 days. **[INFERENCE: arithmetic with fixed ceilings, all clean holds and certificates ready on time; no empirical claim that real core gaps will be one or two counts.]** If nights reveal larger gaps, apply the same rule and report the larger cost; do not silently truncate refinement and call the per-core goal complete.

At the existing 5.38 h certificate size, fourteen coarse steps schedule about 75 h of qualification execution; sixteen or thirty-two core steps schedule about 86 h or 172 h more, before controls, mapping, requalification, overhead and repeats. **[INFERENCE: unchanged matrix sizing, not measured hardware throughput.]** Exact-profile certificate reuse and preparing candidates during holds keep nights from needlessly repeating unchanged local cells. They cannot remove the linear daily-hour floor. The successor gate must include actual per-core truth and report the two phases separately; if useful refinement needs too many serial holds, D has a tractability failure to submit for an explicit policy decision, not permission to shorten holds or relax coverage.

## 9. Safety and activation

D deliberately permits exploratory profiles whose daily CO-added hazard may exceed λ*. A 50-hour survival can occur with about 61% probability even at a true rate of one failure per 100 h. **[INFERENCE: `exp(−50/100)` under a stationary Poisson model.]** U+5, qualification and stopping are safeguards, not rare-event guarantees. Hidden workloads, tail flattening, thermal history, misleading locality, shared voltage, interactions, firmware changes, missing evidence and incorrect answers can defeat transfer from nights. Newest-step blame can stop an innocent unit while an older one remains hazardous. A watchdog aids recovery; it does not prevent data corruption, a freeze or every failed reset.

**No automatic nonzero day offsets until all three conditions hold:** (a) D's rules and machine allowance are approved and specified; (b) the pre-registered successor gate scores that exact policy and its acceptance is met or an explicit approval disposes of any failure; (c) the hardware acceptance below passes. Stock-only work on #1169, #1221, #1218 and #1235 can proceed without authorizing D. A failed gate or merely executing a score is not permission to enable nonzero use.

Hardware acceptance must be recorded against the exact approved revision/configuration, with BIOS CO 0 and no invented measured-time claims:

- Finish the applicable outstanding observer/watchdog/placement checks, including per-core/slot mapping, counting watchdog, persistent day evidence and supported sleep modes. Verify the all-zero baseline and clean stock controls.
- Demonstrate night mapping/bracketing and labelled U/C; demonstrate an exact proposed-profile matrix and its outstanding/failure handling. Record launch/teardown overhead and required repeats rather than assuming an eight-hour simulated night fits.
- Under a separately explicitly authorized **supervised acceptance procedure**, apply a small already-qualified profile through intent/write/readback; stop/abnormal-exit/emergency-zero, restart without promotion, suspend/resume and pause/expiry must match #1218/#1235. This supervised procedure is not automatic day exploration and cannot bypass the stock-only activation gate by calling it a demonstration. Fake injection tests prove crash/firmware paths; do not induce a real daily-use crash or flash firmware to spend the allowance.
- Show a real verified daily hold, an eligible cold-boot decision, correct evidence/allowance notices and retained state after restart. Keep autonomous nonzero rollout disabled until the complete acceptance record passes; document exactly what the supervised procedure authorized and what it did not.
- Complete a separate per-core mapping/application/readback/interaction check before enabling core refinement. A passing uniform-CCD hardware check does not establish nonuniform-core correctness or runtime/BIOS equivalence.

## 10. Journal, schema and architectural record

The pure tuner owns these decisions; applying services and renderers must not invent a second policy. Folding raw journal observations must deterministically reconstruct the same next candidate, history, adverse-evidence retreat, permanent stops, allowance and status.

The durable journal and reporting contract requires:

- stable unit identity and kind/phase/topology mapping; full per-core actual profile, not a sparse vector or averaged CCD approximation;
- U's unknown/observed/censored state and C/margin witnesses; retain old `ccd.edge`/`ccd.cap` wire meanings for historical decoding instead of renaming them into per-core facts;
- exact qualification matrix/content/firmware/epoch identity and per-cell raw witnesses;
- ordered successful step/application identities, changed unit, before/after vector, clean-hold interval witnesses, inherited ancestry and consumed retreat cursors;
- unit stop/finish reason, first-backstep witness and immutable campaign membership;
- a campaign-level machine allowance, unique event identities, provisional/reclassification history, consumption and exhaustion, with no silent renewal;
- permission mode, S's diagnostic-only status, applied vs saved/proposed profile, current-profile daily hours/crashes and separated learning/post-finish events.

Use additive typed events/fields where their semantics genuinely are additive. A new event kind alone need not require SCHEMA under the existing journal contract, but replacing the required CCD profile shape, retyping fields or changing released meanings can. Inventory all strict readers, importers, snapshots and the day applier's current uniform-CCD consistency check before choosing a representation. A future core-phase applier must validate the complete core vector against topology, not disable validation or pretend a nonuniform profile expands from one CCD offset. **RULESET/SCHEMA changes require separate compatibility authorization.** Evidence epoch changes only if workload/credit contracts actually change; using unit-generic policy is not permission to relabel incompatible trials.

**Successor transition:** archive the Ruleset 13 session unchanged and start at verified stock. Preserve original-identity raw daily observations, applicable whole-profile vetoes and compatible night observations for explicitly checked recomputation; never carry S, a qualification conclusion or nonzero D as permission. Reconstruct campaign allowance/stops across transitions rather than using session freshness to renew risk. Old-context facts stay history/hints. If required campaign history cannot be reconstructed, block new nonzero exploration pending an explicit policy decision, not a default zero-consumed counter. The reset boundary is the archived predecessor and durably identified successor session at verified stock: interruption must resume that same transition, not create another campaign. Imports and replay retain source observation/event identities, so an interrupted derived commitment cannot duplicate credit, charges or steps. [Journal transitions](journal.md#transitions) own archive durability and compatibility refusal; the successor implementation must establish its crash-resume boundary before switching compatibility.

The consolidated architectural rationale and predecessor dispositions are [ADR 0094](../adr/0094-two-ends-bounded-exploration.md). The original Ruleset 13 gate remains failed; [#1457](https://github.com/shgew/togi/issues/1457) owns successor registration.

## 11. Shared scoring/reporting interface

D exposes actual applied history and raw witnesses, not simulator truth. Per unit: phase/identity, inherited starting depth, D/U/C, each promotion/retreat, current hold, qualification identity/outstanding cells, stopped/finished/held reason and resulting depth. Per machine: campaign allowance/consumed/remaining, unique classified events with provisional/reclassified state, event identity and phase, escalation actions, applied-profile hours and post-finish events. Certificate and allowance states must be recoverable even if a derived commitment was interrupted.

The scorer independently adds simulator-truth CO-caused vs background crashes and joint hazard. Report unique machine crashes, not a sum of per-unit charges; distinguish **learning**, **post-finish**, **policy-classified CPU-suspect**, **provisional**, **later reclassified** and **truth CO-caused** quantities. A crash-free but unfinished horizon-complete run is not right-censored; actual truncation, harness failure and blocked/unfinished progress have separate outcomes. A stop at the first backstep or allowance exhaustion is a policy result, not automatic useful-depth success.

The successor gate freezes criteria before scoring D, rewards actual benign depth (stock forever must fail utility), removes the −50/−45 contradiction, evaluates joint-profile admissibility rather than coordinate distance from one arbitrary tie-broken optimum, and includes per-core truth now. The rule is that the actual per-core policy receives its own later registration; per-core-truth fixtures now must not be presented as a score of an unimplemented core policy. λ* remains fixed; any sweep is exploratory only. Use `just bench-day-night` directly, not a research-dispatcher campaign or infrastructure changes. Holdout remains sealed until its pre-registered final confirmation. The original Ruleset 13 registration and failing results stay frozen.

