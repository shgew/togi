# Re-derive R7 chains after a backoff

Supersedes the clause of [ADR 0038](0038-self-sufficient-cores.md) that freezes a partial's loaded set once it starts and re-derives only unstarted parts after a profile change. Everything else in 0038 stands. The rule is in [Together trial sequence](../spec/tuner.md#together-trial-sequence) and [Checking](../spec/tuner.md#checking). [Issue #489](https://github.com/shgew/togi/issues/489) records the change and [#493](https://github.com/shgew/togi/issues/493) the decision (Q23, option a); it builds on [ADR 0054](0054-two-phases-replace-the-deepening-loop.md).

## Context

An R7 checking step runs each CCD's full part, then a chain of partials that idle the top requesters in turn. Each partial's loaded set comes from its predecessor's load under the request order of the profile at the time. ADR 0038 froze a partial, and every part before it, as soon as one trial of it had run, and re-derived only parts that had not started.

A backoff inside a cycle changes the profile and can change the request order. A part that had already run kept the loaded set of the old order, the chain below it was never derived again, and the cycle counted its old-order parts as complete. In the trace that raised #489, a core on the first CCD moved from -24 to -21 through three one-count backoffs after that CCD's chain had passed. At -21 it requests more than another core, so the riskiest load of the final order, the first CCD's AVX2 load on cores 1, 2, 3, 5 and 7, was never required. The cycle passed, and the final profile's worst R7 hazard was 7.79 per hour.

#493 settled the answer in Q23: keep the validity rule, where a failure inside a cycle steps the core back and reruns the failed class `n` times, and re-test only the parts whose loaded set changed.

## Decision

- **A part is current only at the profile it was derived at.** Inside an open cycle, a chain part whose recorded derivation profile differs from the checking profile is stale, whether or not it has run. Stale parts re-derive from their predecessor, from the full part down, before the step counts. Several backoffs before the scheduler returns to the step cost one derivation.
- **Re-testing follows trial class.** A re-derived part with the same loaded set is the same trial class, so its passes since the cycle start still count and it runs no new trial. A part with a different set is a new class and needs its own three short and one long passes, under the ordinary evidence rule: an equal-or-deeper pass of that exact set earlier in the cycle counts. A set that left the chain is no longer a cycle requirement; its passes stay ordinary evidence.
- **A cycle passes only with current chains.** Every chain must be derived at the ending profile and every part must pass, so the ending profile's riskiest load has run.
- **The validity rule is unchanged.** A failure still backs the core off and queues the failed class's reruns, which run before the cycle resumes even when that class has left the chain.
- **No new journal surface.** The state is a fold of existing events: `checking.chain` records the derivation `profile`, and a later record of the same `part` replaces it. A part is current only when it and every part before it was derived at the checking profile, so the parts after a replaced one stay stale until they re-derive in turn. The started flag, the only chain state that read `trial.intent`, is removed, so a resume equals a fresh fold. `journal.Schema` stays 4 and `tuner.Ruleset` stays 10 until the ruleset bump of [#530](https://github.com/shgew/togi/issues/530). The derivation message says it re-derives after a profile change and whether the loaded set is unchanged or changed; no field carries that.
- **Whole-profile staleness.** A backoff on one CCD re-emits the other CCD's unchanged nodes. That costs events, never trials.

## Considered Options

- **Keep the freeze (today):** rejected. It lets a cycle pass without the load the final order makes riskiest.
- **Restart the cycle on any failure or backoff (#493 Q23, option c):** rejected by the owner. It multiplies cycle cost by the number of backoffs and discards passes that still cover the profile.
- **Count a cycle clean only when it had no backoff (#489's second proposal):** rejected for the same reason as option c.
- **Compare only the CCD's own cores for staleness:** not adopted. It would save re-emitted events, not trials, and replace a one-line comparison with a new one.
- **Require a changed-set part to run at the final profile:** not adopted. A pass of the exact class at an equal-or-deeper profile is evidence under the spec's Evidence rules for every part.

## Consequences

- Extra trials run only for changed sets: at most four per changed part per R7 step per re-derivation.
- A saved retry of a stale part is dropped with its requirement; an unchanged re-derived set then runs a fresh trial of the same class.
- Phase-2 rounds run with no open cycle and end on a failure, so they never re-derive. The confirmation cycle's reverts change the profile inside the cycle, so its chains re-derive and its passed end confirms a profile whose chains ran in final order.
- Escalation ([ADR 0053](0053-escalation-only-located-hunts.md)) is unchanged: an unchanged part runs no new trial and a newly required set is a new load with its own count.
- A development build that resumes a ruleset-10 journal re-derives stale started parts at its next visit to the step; the Ruleset 11 transition of [#530](https://github.com/shgew/togi/issues/530) starts a seeded session for real users.
