# Activate permitted day profiles on the on-machine rules

Status: accepted, 2026-10-31. Amends only the successor-score and supervised-procedure activation gate in [0094](0094-two-ends-bounded-exploration.md). [Ruleset 14 §9](../spec/tuner-ruleset14.md#9-safety-and-activation) owns activation and hardware acceptance; [runtime](../spec/runtime.md#ruleset-13-policy-boundary) owns application and lifecycle safety.

## Context

Ruleset 14 decides a permitted day profile D, but the stock-only day writer withheld every nonzero proposal. ADR 0094 required the successor simulator score and a separately authorized supervised hardware procedure before automatic nonzero day use. The owner reversed that activation route on 2026-10-31 in [#1458 §9 and its owner decision record](https://github.com/shgew/togi/issues/1458); [#1521](https://github.com/shgew/togi/issues/1521) implements the application cutover.

A successor score is research about the policy, not the machine's permission to apply an eligible profile. The first live step cannot receive its on-machine check before the step is eligible. Neither fact removes the bounded-experiment risk or proves a rare-event reliability claim.

## Decision

Apply the exact committed permitted D, including nonzero offsets, through the existing receipt-checked transaction: durable intent, hardware write and matching all-core readback. Only verified application commits a step. Keep the stock baseline, armed/counting PID 1 watchdog, topology and unresolved-defect refusals, host lock and journal durability contract. No RULESET or SCHEMA change is part of this activation.

Every zeroing authority remains: pause writes and verifies CO 0; stop zeroes and reads back before its clean marker; abnormal exit uses zero-only emergency recovery; journal failure uses the separate emergency-zero path. Failure is not a successful application, and neither recovery nor failure recording can be promised when hardware or journal access itself fails.

Same-boot service restart, configuration-switch restart, sleep resume, early pause resume and pause expiry reapply the still-permitted committed D without promotion. Promotion is only an eligible cold-boot decision. A cold boot starting paused applies stock and cannot promote later in that boot. Safety retreats and firmware reset retain priority over reapplication.

The activation change must prove nonzero application and lifecycle behavior with fake-SMU and VM tests. Successor simulator scoring under [#1457](https://github.com/shgew/togi/issues/1457) remains research and gates nothing; executing or passing a score is not hardware acceptance.

Hardware acceptance is the first live nonzero step's on-machine check, recorded against the exact revision/configuration in that night's [run retrospective](https://github.com/shgew/togi/issues/260): applied-profile all-core readback, stop-to-zero before the clean marker, same-boot restart with the same D and no promotion, suspend/resume with the watchdog counting again, and pause/resume with the still-permitted D. It replaces the supervised procedure, does not block releases beforehand and carries no `needs-hardware` label. Do not induce a real daily-use crash or flash firmware to exercise failure paths. Separate per-core mapping/application/readback/interaction acceptance remains required before core refinement.

## Considered options

- **Keep the successor score as an activation gate.** Rejected by the owner: activation is bounded by the on-machine rules; scoring informs future policy rather than authorizing an individual machine's D.
- **Require a separate supervised application before normal activation.** Replaced by the first live step's on-machine check; it does not invent permission for an otherwise ineligible profile.
- **Bypass the transaction or relax the policy to obtain a live step.** Rejected: qualification, holds, ceilings, event stops, allowance and runtime safeguards remain unchanged.

## Consequences

The day service now applies permitted nonzero profiles rather than silently resetting them to stock. The U+5 ceiling, 50-hour exact-profile holds, exact night qualification, newest-step stopping, whole-profile vetoes and two-event machine allowance still bound exploration. They do not guarantee no freeze, corruption or failure to reset, and displayed evidence remains experimental.

All other decisions and rejected alternatives in ADR 0094 stand, as separately amended by later records. Ruleset 13's original registration remains failed; successor research cannot retrospectively accept it. Existing hardware obligations are not erased, and a passing uniform-CCD check does not establish nonuniform-core correctness or runtime/BIOS equivalence.
