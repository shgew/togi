Status: see [the index](README.md).

# Probe reproduced singleton failures

## Context

[Issue #106](https://github.com/shgew/togi/issues/106) identifies the singleton version of the cost that [ADR 0020](0020-hunt-and-refine.md) already avoided for combinations: an idle limit several counts shallower than a loaded limit costs another hunt, with crash reboots, at every count. Its recorded Ruleset 10 `idle-limit` comparison had 16 culprit hunts and 96 crashes in each of eight simulated runs, with two cores moving eight counts each in eight separate hunts. Those are historical Go-simulator results, not measurements of the current Rust implementation.

Decided by the owner on 2026-10-17: include singleton probing in Ruleset 12, between a reproduced singleton failure and a known safe anchor, and use `tools/bench/machines/idle-limit.json` as the cost regression. This supersedes ADR 0020's singleton commitment at the original failing offset followed by one count of backoff; it does not change direct attribution, combinations or the ruleset/schema constants in this layer.

## Decision

Use the existing hunt groups, probe payload and member-probe gallop/bisection for a one-member set. [Hunt](../spec/tuner.md#hunt) owns the exact eligibility, evidence windows, offsets and commitment rules.

Acceleration needs a live failed singleton group, not just a singleton left after passing complements or a known-failure inference. The other cores stay at that group's parked offsets. This preserves the distinction between observing the original profile fail and reproducing the failure with one suspect retained.

The parked offset is a candidate anchor, not assumed stability. Reuse the ordinary ledger's `n` valid passes at the fully parked profile in the reproduced singleton's exact class under the current backend. A passed locate supplies them when its class matches. If no such evidence exists, explicitly test the parked anchor with a probe group before any interior probe. A locate at a different duration cannot silently authorize the bracket. A failed or skipped anchor ends probing conservatively; an actual all-zero failure is the existing `failure_at_zero` claim, not a safe endpoint.

Once the anchor passes, gallop by 1, 2, 4, … counts from the reproduced failure and then bisect the remaining bracket. Keep the shallowest reproduced failing offset in `hunt.end.members`, and commit the failure point there, retaining any shallower existing point. Back off one count past that point, never deeper than the current offset. A passing experimental profile is not a passing full tuning profile: the original failure still queues the ordinary reruns.

The journal says which probe establishes the anchor and which probes narrow the bracket; their exact profiles, trial classes and outcomes remain ordinary hunt facts. The end cites those groups and failures. No per-core singleton counter, cross-hunt cache, new event type or history query is introduced. Reset cancels the active hunt; replay reconstructs the same next probe and emits its missing commitment once.

## Alternatives

- **Keep one count per singleton hunt:** rejected for reproduced idle hazards; it pays the full localization cost repeatedly for a count boundary that can be tested within the open hunt.
- **Treat CO 0 or a parked full-cycle offset as automatically safe:** rejected. Background instability can fail at zero, and a cycle's class and pass budget need not establish this hunt's exact class. The anchor must have qualifying evidence or be tested.
- **Require pre-existing anchor passes and otherwise keep one-count hunts:** rejected. Ordinary hunts need not collect a full set of fully parked passes, so this can make the advertised acceleration unreachable. Explicit anchor acquisition closes that path.
- **Accelerate merely because one candidate remains, or after direct attribution:** rejected in this decision. The singleton must have reproduced under the parked background before its count bracket is searched; direct failures retain their existing attribution and zero-offset handling.
- **Build a separate singleton search state machine:** rejected. The existing one-member probe plan already provides the gallop, bracket and stop semantics with journal-derived active-hunt state.

## Cost evidence

Compare the current Rust parent and candidate with the same `idle-limit` machine and dev seeds. Report conclusion status, final per-core offsets/total depth, simulated hours, crashes, hunt count, and hunt groups split into locate/narrowing/anchor/interior probes. The intended saving is fewer repeated localization groups and crash reboots, not merely relabeling hunts as probes. Retain each journal so the anchor passes and the shallowest singleton failure can be inspected. No current Rust cost measurement is claimed by this decision record.
