# Report qualified rotations instead of durability tiers

Supersedes the tier parts of [ADR 0020](0020-hunt-and-refine.md): the Bronze requirement attached to a done, fully refined profile and a clean qualifying rotation; Silver and Gold's clean-hour thresholds and tier clock; and the clock's zero-failure bound. It also supersedes the tier parts of [ADR 0024](0024-schedule-from-uncontradicted-evidence.md): crediting an earlier rotation for Bronze, emitting a tier decision that cites that rotation, and retaining Silver and Gold's unchanged clock and clean-hour thresholds. Its earlier uncontradicted-rotation credit for `run --rotations` remains.

A durability label conflates workload breadth, testing time and a claim about future failures. A qualifying rotation covers the configured workloads, but a finite number of passing starts cannot prove stability. Selecting a failure-free window after failures and reading its bound repeatedly does not make that window an unconditional or sequentially valid assurance. The operator needs the recorded testing evidence and a way to choose how long to keep testing, not a rank.

## Decision

- Remove Bronze, Silver, Gold and the unavailable Platinum rank, the tier clock, clean hours, per-regime and per-workload time exposure, and failure-rate bounds. Remove `tier.change` outright, including its payload decoder and styling. Do not replace it with a display-only label.
- Keep guard rotations, qualifying coverage, valid per-workload starts, qualified-rotation evidence, refinement and the `run --rotations N` stop rule. A count still requires clean qualifying rotation ends valid for the current profile, with every core done; stopping additionally requires that refinement can reach no more total depth. Eligible earlier uncontradicted rotations at equal or deeper profiles still count. The session stops before starting another rotation.
- `status` reports qualified rotations since the last deepening: the valid count, including eligible earlier credit, and the latest qualified rotation's number. Missing qualifying coverage and valid starts remain visible.
- Measure the Tctl peak over passed resident trials since the resident profile last changed, using the latest `profile.change` as the boundary. Show its source trial-end event. A profile change clears this window even when it is a shallow backoff; temperature is diagnostic, not qualification evidence.
- Read-only commands treat historical `tier.change` events as opaque and facts extraction skips them. A writable older-ruleset or older-schema journal containing the removed kind is archived without appending; a current-ruleset, current-schema journal with unknown kinds is still refused.

This removal ships with the ruleset-7 transition in the lower layers of the same stack and does not bump `tuner.Ruleset` separately. It must merge before ruleset 7 is released, or the ruleset must be bumped again. Existing journals are archived rather than rewritten to erase historical events.

## Considered Options

- **Keep tiers as display-only:** rejected. Even without a tuner decision, the labels still imply a durability ranking and require the same clocks, thresholds and exposure bookkeeping.
- **Keep clean hours and bounds without labels:** rejected. They retain the selected-window interpretation and repeated-reading caveats while adding no workload coverage beyond the retained starts and rotations.
- **Stop automatically at a fixed testing duration:** rejected. The operator chooses a rotation count or indefinite guard; elapsed time cannot replace qualifying coverage, done cores or completed refinement.
- **Remove qualification along with tiers:** rejected. Qualification is the workload-breadth evidence used by guard, refinement and the existing stop rule, not a durability rank.

## Consequences

The profile and its marks still determine search, hunts and refinement. Guard qualification and stopping are unchanged; only the rank, time accounting and bounds disappear. `run --rotations N` is the explicit choice of how long to keep testing, and a qualified-rotation count is evidence about tested workloads, not a guarantee for future real use. The temperature window now follows profile changes independently of qualification and failure accounting. Historical ADRs and changelog entries retain their original record, with superseded decisions identified above.
