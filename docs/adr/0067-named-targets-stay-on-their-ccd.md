# 0067: Named targets stay on their CCD

Date: 2026-10-17

Status: Accepted; amends ADR 0038's named-core voltage-target selection. The clock tolerance remains a candidate for the integrating agent's bench before merge.

## Context

[#688](https://github.com/shgew/togi/issues/688) records core 11's failures in trials 0106–0119 of session 20261010T230611Z. A carried all-core target was set by another CCD; pooling four CCD1 passes with that unrelated load supplied the fifth pass. Failures without clocks bypassed the clock filter. Other failures rejected full-CCD passes only a few MHz below the failing load.

The owner decided to use loads wholly on the named core's CCD and the failing load's last measured clock when the failure has none. On 2026-10-17 the Ruleset 12 integration design additionally excludes targets when neither clock exists and counts passes separately per load and exact profile.

## Decision

The [canonical target rule](../spec/tuner.md#r7-voltage-targeted-backoff) owns eligibility. A named core may borrow evidence from other loads containing it, but only within its own CCD. A two-CCD all-core pass is not such a load; an all-core pass on one CCD is. Top voltage is computed on the named CCD, never on the other CCD's request lane. Different durations of one load can supply the required passes; different loads and exact profiles cannot. Ordinary class failure invalidation and backend identity still apply.

Retain only the newest measured clock per load and CCD under a backend identity, independently of request lanes. Empty reports preserve it; resets of a loaded core remove it. When folding a failure, freeze its own clocks supplemented by that same load's earlier clocks, and cite fallback sources. A later measurement cannot reinterpret the failure. Profiles do not erase the latest clock: this is measured load-clock telemetry, not a passing-profile witness. Target witnesses still require their own exact profile and valid evidence.

Use an inclusive **25 MHz** tolerance. Zero tolerance cannot meet the issue's first-backoff reconstruction: trial 0106 ran at 5040 MHz, while its qualifying full-CCD profile's passes ran at 5024–5029 MHz. The issue also records a 23 MHz rejection (5397 versus 5374 MHz) and a 2 MHz rejection (5397 versus 5395 MHz). Both differences fall inside this boundary; 25 MHz qualifies and 26 MHz does not. These observations motivate this simple method constant, not proof of physical clock equivalence or a fitted tolerance. The simulator comparison measures its consequences before merge.

## Alternatives

- Zero tolerance: conservative but necessarily leaves trial 0106 without the target required by acceptance; rejected for this candidate after reading the recorded values.
- Same-load-only targets: the failing partial had no passes, so this cannot supply the needed evidence.
- Pool loads at one profile: unrelated loads cannot establish the repeated-pass evidence of a particular load.
- Skip filtering without clocks: recreates the low-clock carried-target error. No current or earlier same-load clock means no target and the ordinary one-count fallback.
- All-core evidence split into CCD lanes: still a different electrical load from a load wholly on one CCD; excluded even when its local top is available.

## Evidence and consequences

The test `reconstructed_0106_to_0119_uses_the_recorded_ccd1_target` reconstructs the target decision inputs from carried seqs 929–934 and the failures' recorded clocks. It retains the exact profiles, source sequences, CCD1 top and named-core request lanes needed by the target index; it is not a literal replay of a Go wire journal, which Rust does not decode. For missing clocks in 0107, 0108 and 0111 it uses the immediately earlier measured failing-load clock. The first target is 1.046082615852356 V, and core 11's measured 1.0315487384796143 V requests five counts. Separate fold tests exercise actual clock fallback, empty reports and resume.

The implementation pull request records tuner tests, the gate and dev-bench comparisons against main. They measure this rule on the declared simulated machines, not its hardware validity. RULESET and SCHEMA are unchanged in this layer; the stack's base owns the single ruleset bump.
