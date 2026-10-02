The Bronze credit, tier-change citation and retention of the tier clock and clean-hour thresholds are superseded by [ADR 0028](0028-remove-tiers.md); earlier qualified-rotation credit for `run --rotations` remains.

# Schedule from uncontradicted evidence

Supersedes the initial hunt duration and partition ordering, and the requirement for a new qualifying rotation after every deepening, in [ADR 0020](0020-hunt-and-refine.md). The hunt anchors, mask evidence windows and tested joint backoffs in [ADR 0023](0023-hunts-that-converge-on-shared-voltage.md), and ADR 0020's tier clock, stand.

Ruleset 5 repeats short masks even when a longer failure followed sufficient short starts, repeats binary localization of a core that already failed successive masks, and repeats a qualifying rotation after refinement returns to a profile that qualified earlier. These costs were measured in the fixed benchmark scenarios before changing the tuner. The simulator, session loop, failure detection, journal format and duration defaults are unchanged.

## Decision

Ruleset 6 uses three conservative, journal-only scheduling rules:

- **Choose the failed duration from uncontradicted short evidence.** A hunt of a longer failed trial can start at that duration only after `n` valid short passes of the same regime, workload and loaded cores covering its failing profile, before its failure and after the latest reset. Any earlier failure at the short duration or less in the regime since reset disables the prior, even on another workload or profile. Short passes choose a duration; they never establish a longer mask's outcome. The first mask cites the supporting passes.
- **Probe a repeatedly attributed masked core first.** With more than two candidates, the most recently marked core can receive the first singleton mask when the two newest eligible attributed masked failures of the chosen class both name it, after reset, at successive one-count shallower offsets. The current failing offset must be one count shallower again. The singleton uses the ordinary mask evidence rules. A pass restarts the binary parts without claiming an untested group passed; a failure keeps its ordinary attribution or hunt outcome. The mask cites both prior failures.
- **Credit an uncontradicted earlier qualifying rotation.** A clean qualifying rotation that ended with every core done can count for Bronze and `run --rotations` after a later deepening if it ended after the latest reset, its ending profile is at least as deep as the current profile on every core, and no failure of any class since reset occurred at an equal or shallower profile on every core. An incomplete failure profile prevents credit. When Bronze relies on this credit, its decision names and cites the earlier rotation end. Rotation coverage still uses passes since its own start; Silver and Gold still need clean hours since the unchanged tier clock.

All choices derive from recorded evidence, without core, CCD or scenario special cases. Failures, marks, offset limits and mark-aware profile validation are unchanged. A ruleset-5 session is archived and seeds a ruleset-6 session with candidate edges and eligible same-BIOS failed marks under [ADR 0019](0019-a-ruleset-change-starts-a-seeded-session.md); joint marks and passed rotations do not carry across that transition. Journal schema remains 2.

## Considered Options

- **Start every hunt at the longer duration after short passes:** rejected. Without the regime-wide short-failure veto, a flat-hazard development run lost its conclusion and depth. Evidence for duration selection cannot hide observed short failures.
- **Treat a passing singleton as a passing binary part:** rejected. The other members have not been tested together at their failing offsets.
- **Always require a new rotation after deepening:** replaced only when the earlier profile dominates and no failure contradicts it. A deeper or incomparable current profile still needs new coverage.
- **Carry the earlier rotation's clean hours:** rejected. Breadth coverage and clean exposure answer different questions; credit cannot advance Silver or Gold's clock.
- **Shorten isolated localization or skip R2 during coarse search:** rejected in the experiments. Faster development results lost conclusions or depth on other fixed seeds.

## Measurement

The promoted implementation was compared with ruleset 5 from `main` at `b7b7af4`, using the unchanged `tools/bench/suite.toml`, both seed splits and 16 workers. All 96 baseline and candidate runs concluded.

| Scenario | Candidate/base time ratio | Mean crash delta |
|---|---:|---:|
| default | 1.001 | 0.000 |
| shared-rail | 0.848 | 0.000 |
| flat-hazard | 0.998 | -0.625 |
| late-onset | 0.903 | -16.875 |
| idle-edge | 0.958 | -36.000 |
| misleading-mce | 1.000 | 0.000 |

The equally weighted overall ratio was 0.949, with a 95% bootstrap interval of [0.944, 0.956]: ACCEPT, with no conclusion, hazard, depth or shared-rail guardrail violation. The development and holdout geometric ratios were 0.945362 and 0.953271. Final profiles matched in 95 of 96 pairs; flat-hazard development seed 3 ended four counts deeper. Maximum hazard did not increase in any pair. Three default runs and one flat-hazard run were slower.

The kept journals show each mechanism: late-onset development seed 1's hunt 2 begins with a 300 s mask citing five earlier short passes; idle-edge development seed 1's hunt 3 begins with core 12 alone, citing its two attributed masked failures; shared-rail development seed 1 earns Bronze at event 12791 by citing rotation 1's end at event 11587 after refinement restores its profile, then shuts down for the requested rotations without running rotation 3.

## Consequences

A singleton that passes adds work before the normal split, and a rare unrelated short failure disables the duration prior for the rest of the session until reset. Both are conservative costs. Finite clean rotations and exposure remain evidence, not guarantees of stability. Hardware performance has not been measured for these rules.
