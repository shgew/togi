# Crashes are not a cost

togi finds edges by running cores past them, and past an edge a core often freezes the whole machine. The tuning boot's hardware watchdog resets it and the next boot resumes the journal. A live session on the target machine recorded ten crash reboots in 21 hours. A search that treats crashes as something to ration gives up depth or time to avoid them: a crash budget per search, a daily cap, a pause after a run of crashes, or a preference for tests that pass over tests that crash.

A crash costs nothing but the time of its reboot. togi only moves offsets within [-50, 0]: a negative Curve Optimizer offset lowers voltage and never raises it, so no offset togi writes can harm the hardware. The offsets are volatile and gone after the reset ([ADR 0004](0004-find-only.md)). The watchdog and the tuning boot bring the machine back unattended ([runtime spec](../spec/runtime.md)), and the journal survives the reset: every event is fsynced before the action it records, and a torn last line is repaired on replay ([journal spec](../spec/journal.md)).

## Decision

- A crash is evidence, like any other failure. togi never limits, rations or avoids the crashes its trials cause: no crash budget for a search, no daily cap, no pause after many crashes.
- A search design weighs a crash by the time its reboot takes, like any other trial time. Between two tests that answer the same question, togi runs the one that answers sooner, whether or not it may crash.
- Crashes that no offset caused are not covered by this decision. A stray crash happens before togi applied anything in its boot, so togi cannot act on it; stray-crash streaks stay a recovery concern.

## Considered Options

- **A crash budget per search, falling back to a wider backoff when spent:** rejected because it gives up depth to save reboots that cost only time.
- **A daily crash cap or a cool-off after many crashes:** rejected because it idles the machine without protecting anything.
- **Prefer tests that pass over tests that crash:** rejected because a crash is often the fastest answer: a freeze in the first seconds shows that a configuration fails, while showing that it passes takes several clean runs.

## Consequences

- Tuning may reboot the machine many times an hour, for example while narrowing down which core causes a freeze. The tuning boot is not meant for other use while it runs.
- Earlier ADRs that count crashes as the cost of a step, such as [ADR 0013](0013-candidate-edges-for-a-new-session.md), are records of their time; under this decision the cost they describe is the time those crashes take.
