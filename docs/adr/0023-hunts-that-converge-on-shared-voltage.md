# Hunts that converge on shared voltage

Supersedes the hunt anchor and the joint-mark backoff choice of [ADR 0020](0020-hunt-and-refine.md); the rest of ADR 0020 stands.

The first ruleset-4 session on the target machine ran about 33 hours of guard, across four runs, without passing its first step, the R7 start on CCD0. Its seven unattributed crashes each started a hunt anchored at all-zero, because no profile had qualified yet. Each completed hunt took 42 to 49 masks, ended with a joint mark over all eight CCD0 cores, and backed off the member leaving the most depth reachable by one count. No trial had ever passed the resulting profile. The resident R7 CCD0 start crashed on its first start all seven times it ran: once before the first hunt and after each of the six backoffs. The journal shows why:

- Any one CCD0 core at 0 rescued the rest: 48 of 48 masks with one core at 0 and seven at their failing offsets passed 5 starts each.
- The failure follows the shallowest loaded offset, as it would if the cores share one voltage that the least undervolted core sets. Per R7 CCD0 start, by the shallowest offset among cores 00 to 07: -31 to -29, 26 of 26 failed; -27, 10 of 12; -26 to -23, 23 of 240; -22 to -15, none of 25; a core at 0, none of 434.
- Edge probes found that core 00 had to move to between -26 and -22 before the combination passed. With core 00 held at its shallowest failing offset, close to that edge, most later members' probes passed one count shallower, which a failure rate near one start in ten allows. The joint mark therefore sat one count from the failure on most members, and the backoff rule moved one of them, leaving core 00 at -31.
- Each of hunts 2 to 6 repeated the same 14 part and complement masks, 70 starts or 2.3 hours, which the previous hunt had already established at an equal or deeper profile. All passed again.

## Decision

Ruleset 5 changes three hunt rules.

- **The anchor is a qualifying profile raised to the failing profile.** Each core takes the shallower of its qualifying and failing offsets. Passes on the qualifying profile cover the raised profile, so it needs no new evidence. Only cores deeper at the failure than in the qualifying profile are candidates. A refinement round that yields a core and deepens others therefore hunts only the deepened cores, instead of falling back to all-zero and every core.
- **A joint mark first backs off to a tested probe.** Among the hunt's passing edge probes that moved one member with every other member at its failing offset, the backoff takes the one moving its member the fewest counts, provided the resulting resident profile reaches no mark, and moves that member to the probe offset. The decision cites the mark and the probe. Without such a probe, the ADR 0020 rule applies: the member leaving the most depth reachable moves one count past the mark.
- **Part and complement masks reuse earlier hunts' evidence.** They count every pass and failure of their class in the session under the ledger's usual rules. Full and edge masks still count only evidence since their hunt started, because edge probes run near the edge, where 19 of 47 later probes failed although earlier evidence predicted a pass.

On a simulated 16-core machine shaped after the session, with a CCD0 joint at -27 that fails within about 20 seconds and one at -23 that fails about one start in ten, sixteen seeds of ruleset 5 and four of ruleset 4:

| | ruleset 4 | ruleset 5 |
|---|---|---|
| first qualifying rotation | 155–200 h | 24–40 h |
| session through Bronze | 189–250 h | 40–57 h, 13 of 16 seeds within 40–42 h |
| hunts | 38–52 | 6–10 |
| crash reboots | 402–467 | 35–64 |
| final profile | core 00 at -22, every other core at its edge | the same on every seed |

On the 25 default simulator seeds, the hunts, crashes and final profiles are unchanged, except for one extra crash on one seed, and six seeds finish 0.2 to 0.4 simulated hours sooner.

## Considered Options

- **Keep the ADR 0020 backoff:** rejected. Probing every member keeps the joint mark accurate only when probe outcomes are deterministic. Near a shared-voltage edge they are not, so the mark rewards whichever member passed by chance, and the backoff lands on an untested profile.
- **The backoff without the raised anchor:** rejected. The anchor rule then required a qualifying profile at least as shallow as the failure on every core. After the first qualifying rotation, refinement yields a core and deepens others, so every later hunt fell back to all-zero and every core. On the shaped machine a prototype without the raised anchor reached the simulator's 1,000-boot limit on four of five seeds.
- **Back off the member that sets the voltage, the shallowest one:** rejected as a rule. It encodes one hardware explanation, while the tested probe follows from the evidence and holds whatever the cause.
- **Move every member to its passing probe:** rejected. It gives up depth on members whose probes passed only because an earlier member was held near its edge.
- **Reuse edge-probe evidence across hunts:** rejected for now, for the reason above.
- **Probe the shallowest member first:** measured no gain on the shaped machine or the default seeds; not adopted.
- **Noisy group testing:** still deferred, as in ADR 0020. These rules bring the shaped machine to 6 to 8 hunts without modeling noise.

## Consequences

- A tested backoff can move a core several counts at once, giving up more depth than one count would. Refinement still targets the deepest profile the marks allow, so depth a backoff gave up can come back through refinement checks.
- Most hunts after the first qualifying rotation have few candidates and a raised anchor that has already passed.
- Ruleset-4 journals transition once to ruleset 5, carrying candidate edges and failed marks as [ADR 0019](0019-a-ruleset-change-starts-a-seeded-session.md) describes; joint marks do not carry.
- `TestSharedVoltageJointBacksOffOnlyTheShallowestCore` replays the pattern on eight simulated cores. Ruleset 4 needs 39 hunts and 200 crashes and ends with the wrong profile; ruleset 5 needs 6 hunts and 27 crashes.
