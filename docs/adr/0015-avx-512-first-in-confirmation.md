# Run mprime AVX-512 first in confirmation

In the first ruleset-2 hardware session on a 16-core Zen 5 part, 14 of 17 confirmation failures were R2 mprime AVX-512 in slot five. Four passing slots ahead of each failure were repeated one count shallower: 14 × 4 × 5 minutes = 4 hours 40 minutes of 18.6 hours of confirmation trials. The other failures were R5 in slot nine twice and R1 y-cruncher SNT + SVT in slot three once.

## Decision

At a candidate edge, each core runs R2 mprime AVX-512 36K-248K first, then the R1 workloads in catalog order, the remaining R2 workloads in catalog order, and R3, R4 and R5. All nine isolated trials must pass. A failure backs off one count and restarts the full set from mprime AVX-512 at the shallower offset. Search and guard retain the workload catalog order. This supersedes only the confirmation order in [ADR 0011](0011-blame-by-load-and-automatic-regain.md).

## Considered Options

- **Keep the order:** rejected because the most frequent observed failure would continue to follow four passing trials on each restart.
- **Lead each restart with the workload that just failed:** rejected because extra state and a replay path offer little additional gain over a fixed order with AVX-512 first.
- **Order every slot by observed failure rate, moving R5 up too:** rejected because two R5 failures on one machine are thin evidence for reordering it.

## Consequences

The confirmation set changes, so the tuning ruleset becomes 3 while the journal schema remains 2: recorded event shapes and their interpretation have not changed. An active ruleset-2 session must be archived with `shycler reset --all` before tuning with this build. `candidate_edges` can start the new session in confirmation at known candidate offsets. R5 remains last, and the nine-trial cost when every workload passes is unchanged.
