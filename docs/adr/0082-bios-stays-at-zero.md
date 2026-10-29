# Keep BIOS Curve Optimizer at zero

Status: accepted for the Ruleset 13 stack; supersedes [0004](0004-find-only.md) and the BIOS-start choice of [0006](0006-start-from-bios-values.md). Retained and consolidated in [0094](0094-two-ends-bounded-exploration.md), which owns current policy and adds the firmware-restoration exception; this record is historical.

The owner chose runtime application at normal boot/resume while BIOS stays at CO 0 ([#1214](https://github.com/shgew/togi/issues/1214)). A watchdog reset can then return firmware to the untuned baseline, and day steps remain attributable to journaled applications rather than an unobserved BIOS edit. [The runtime policy boundary](../spec/runtime.md#ruleset-13-policy-boundary) separates decisions from the applying service's safety lifecycle.

There is no export logic or automatic instruction to copy a confirmed profile into BIOS. An evidence view may inform a user's own judgment, but runtime/BIOS equivalence is assumed, not shown, and early-boot safety is not established by a runtime trial. Firmware changes invalidate active tuning evidence under [the tuner rule](../spec/tuner.md#firmware). The service's watchdog-arming decision belongs to the applying layer, not this ADR.
