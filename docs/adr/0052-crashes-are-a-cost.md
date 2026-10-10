# Crashes are a cost

Supersedes [ADR 0018](0018-crashes-are-not-a-cost.md). [Issue #493](https://github.com/shgew/togi/issues/493) records the decision (Q12); [#526](https://github.com/shgew/togi/issues/526) absorbed the issue that asked for this ADR.

## Context

togi finds edges by running cores past them, and past an edge a core often freezes the whole machine. The tuning boot's hardware watchdog resets it and the next boot resumes the journal. ADR 0018 held that a crash costs nothing but the time of its reboot, and so told a search design to weigh a crash by that time alone.

That left crashes unmeasured. Ruleset 10's located hunts ([ADR 0040](0040-located-hunts.md)) run a trial on purpose where the failure probably recurs, and the median run on `target-shared-voltage` (24 seeds, [#493](https://github.com/shgew/togi/issues/493#issuecomment-6040930168)) took 426.5 crashes. [#493](https://github.com/shgew/togi/issues/493) decided that fewer crashes is a goal of the design and a criterion of its gate (Q12), and that a new ADR replaces ADR 0018.

ADR 0018's other points hold. togi only moves offsets within [-50, 0], the CPU's own limits stay in force, the offsets are volatile and gone after the reset ([ADR 0004](0004-find-only.md)), and the journal survives the reset ([journal spec](../spec/journal.md)).

## Decision

- A crash is evidence, like any other failure.
- A crash counts as a cost when choosing tuner rules, beside time. Between rules that end at comparably deep profiles, fewer crashes is better, and a rule that adds crashes has to show what it buys.
- The median crash count of a scenario's runs is a criterion of the Ruleset 11 gate, scored as [#530](https://github.com/shgew/togi/issues/530)'s Acceptance records it: median crashes on `target-shared-voltage` below ruleset 10's 426.5. The gate's registration in `tools/bench/suite.json` is not changed by this ADR; a later change to it needs its own decision.
- Crashes that no offset caused are not covered by this decision. A stray crash happens while no togi offset is in force: before togi applied anything in its boot, or after it restored the baseline ([tuner spec](../spec/tuner.md)). togi cannot act on it, and stray-crash streaks stay a recovery concern.

This ADR adds no crash budget, daily cap or pause to a running session: the cost enters when rules are chosen and gated, not as a limit on the session.

## Considered Options

- **Keep ADR 0018:** rejected. Weighing a crash by its reboot time alone leaves the owner's goal, fewer crashes, unmeasured and ungated ([#493](https://github.com/shgew/togi/issues/493), Q12).
- **A runtime crash budget, daily cap or cool-off:** not adopted. ADR 0018 rejected them because they idle the machine or give up depth to save reboots, and this decision brings no evidence against that. Adopting one would be a new decision.

## Consequences

- A tuner change that raises the crash count needs the evidence that the added crashes buy depth or safety. The bench already reports `crashes` per run and its median per scenario ([benchmarking](../benchmarking.md)).
- Tuning may still reboot the machine many times an hour, for example while narrowing down which core causes a freeze. The tuning boot is not meant for other use while it runs.
- Earlier ADRs that count crashes as the cost of a step, such as [ADR 0013](0013-candidate-edges-for-a-new-session.md), are records of their time. Their step costs are still true as time; this decision adds the cost they did not weigh.
- [ADR 0053](0053-escalation-only-located-hunts.md) is the first rule chosen under it.
