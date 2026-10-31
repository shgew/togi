# The operator attests stock stability; no all-zero trial

Status: accepted with Ruleset 14, decided in [#1512](https://github.com/shgew/togi/issues/1512) on 2026-10-31. Partly reverses [0094](0094-two-ends-bounded-exploration.md)'s retained stock controls. [Ruleset 14](../spec/tuner-ruleset14.md#operator-attested-stock-stability) owns the policy; [runtime](../spec/runtime.md#night-and-day) and [journal](../spec/journal.md#stock-attestation) own confirmation, refusal and durability.

## Context

The first reported Ruleset 13 night spent 8.4 hours on 257 passing all-zero
control cells, followed by 3.5 hours mapping at offset 0. These are observations
from the [reported run](https://github.com/shgew/togi/issues/1512), not a general
runtime estimate. A stock-only trial changes no factor togi controls and costs
machine time before the first nonzero experiment.

0094 retained first-night and changed-firmware stock controls, stock-confirmation
reruns and controls before firmware restoration. The operator instead accepts
responsibility for establishing stock stability before tuning.

## Decision

The existing night confirmation includes the operator's assertion that the
machine is stable at CO 0 with BIOS at stock. A yes is journaled once per
firmware context as the additive `stock.attested` kind. BIOS, AGESA or microcode
changes require a current-context attestation; a run started another way cannot
silently assume one. No separate prompt is added. Released event shapes and
`SCHEMA` remain unchanged, and this ships in the unreleased Ruleset 14 switch
without a separate `RULESET` bump.

Consent is session-scoped: an explicit reset or ruleset transition requires a
new answer in the new session, not an import of archived operator consent.

No trial runs with every core at 0: remove stock controls, mapping's tested
zero step, stock-confirmation reruns and all-zero filler or requalification.
Mapping begins at −5 with 0 implicitly the last passing bracket endpoint.
Isolated mapping still leaves the other CCDs at 0. A failure located on a CO-0
core during isolated mapping counts against the tested CCD; whole-profile
qualification and reproduction retain exact-profile failure semantics.

A night without a current attestation, or with no admissible nonzero cell,
ends with a named reason and every core at stock. Verified baseline readbacks,
day observation at stock, exact-profile qualification and the 50 clean stock
daily hours before initial promotion or firmware restoration remain required.
They do not require an all-zero workload launch.

## Alternatives rejected

- **Retain all-zero controls as a prerequisite.** They consume tuning time while
  testing a baseline the operator is responsible for establishing.
- **Keep a special zero rerun for a failure naming a stock core.** The location
  does not prove that a nonzero isolated experiment was uninvolved; the tested
  CCD receives the conservative charge without spending time on a stock trial.
- **Assume consent when tuning starts without the night command.** A menu-started
  boot or direct run is not the operator's stock-stability assertion. Refusing
  nonzero work preserves that distinction.
- **Add a second confirmation or change a released event's payload.** One
  existing confirmation and a new additive kind keep the interaction simple
  and preserve released wire contracts.

## Consequences

Togi no longer measures stock stability with stress workloads. An incorrect
operator assertion can contaminate mapping and lose useful depth; the
attestation is an operational prerequisite, not a measured safety certificate.
Daily stock observations remain visible, and stock is still the verified
fallback and restoration target. Existing hardware, containment, preflight,
thermal and journal-failure safety stops remain in force.
