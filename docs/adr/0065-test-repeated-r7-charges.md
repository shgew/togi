Status: superseded by [0080](0080-per-ccd-first.md); no repeated charges, requester probes or remembered locate results remain. Honest partial loads remain direct coverage, not attribution.

# Test repeated R7 charges before rerunning the full load

Amends [ADR 0053](0053-escalation-only-located-hunts.md) and [ADR 0064](0064-locate-when-passes-clear-the-loaded-cores.md): a locate ending `loaded` is not repeated without a reason to rearm it. Extends [ADR 0055](0055-rederive-r7-chains-after-a-backoff.md) with diagnostic request-order partials before full-load reruns. [#959](https://github.com/shgew/togi/issues/959) includes folded [#927](https://github.com/shgew/togi/issues/927) and the stall-evidence design of [#895](https://github.com/shgew/togi/issues/895). The owner selected all three for Ruleset 12 on 2026-10-17.

## Context

The [runs 4–5 retrospective](https://github.com/shgew/togi/issues/260#issuecomment-6231521263) found 27 unattributed R7 backoffs taking core 03 from −36 to −1. None of the session's 102 R7 failures named it. Raising its offset raised its request, keeping it top and making it take the next charge. The CCD 0 AVX-512 load failed 7/49 trials with core 03 at −14 and 3/18 at −4 through −1, with its other loaded cores unchanged. Its first partial without core 03 named core 00 after one second, then core 02 in another trial. Ordinary checking derives those partials only after the parent load passes its requirements, too late to interrupt this loop.

Passes were interleaved with the charges. Trials 0843 and 0849 failed around five passing trials (0844–0848), so a threshold reset by any parent pass would miss the same ratchet. This is a different question from locating an unloaded cause: the existing locate-escalation count still resets on a pass.

The first Rust run also repeated ten locate hunts of a load whose locate kept failing at once. The loaded-core backoff after each locate raised the escalation count again, although it changed none of the unloaded offsets that the locate had already tested at 0.

Stalls are not measured attribution. In the later run, eight crashes of cores 09, 10 and 13 stalled on core 09 while six charges moved top requester 10; a subsequent computation error named 09. That warrants an experiment retaining 09 without its higher requester, not an assumption that a stalled worker caused a whole-system hang.

## Decision

The [tuner spec](../spec/tuner.md#testing-a-repeatedly-charged-requester) owns the exact rule.

- After a core's second live unattributed checking charge for the same workload and loaded set, run the part idling its whole tied request group before the full load's backoff reruns. Keep the count across parent passes. Clear it when that probe completes or fails, or a loaded core resets. The count measures repeated untested blame, not failures since a pass.
- A loaded stalled core outside that group on the charged CCD accelerates the same experiment after one charge. It changes probe timing, never attribution. No telemetry is required; frozen offset order remains a usable way to select an experiment.
- All-core loads get a separate experiment for each charged CCD that reaches the threshold, leaving the other CCDs loaded. These are ordinary together partials, not all-core checking chains. Idle an entire tied group; omit an experiment leaving fewer than two loaded cores. Splitting a tie would retain another equal requester and weaken the intended intervention; single-core R7 belongs to different failure rules.
- Use `n` fresh valid live passes at the failed **intended** duration. A shortened crash summary must not shorten the experiment. First failure ends it and follows the partial's own ordinary attribution. Success resumes full-load reruns, not a verdict of guilt against the idled group. The original two charges remain; this rule does not regain offsets or erase failure points.
- Remember a `loaded` locate result for exactly its workload and loaded set. Suppress another backoff-count or covered-pass locate until a pass of that load, any unloaded tuning-offset change, or reset. Moving only loaded cores does not rearm it. A pass reopens the distinction because an intermittent failure may now depend on an unloaded core; an unloaded-offset change changes the side previously tested at 0. The all-zero diagnostic before a dead end remains mandatory.
- Rebuild everything incrementally from existing events. Keep at most one previous charge and one pending experiment per `(load, charged core)`, and one remembered loaded result per load. Probe intents are existing together checking reruns; causes and the charge reason explain their differing load. No new journal fields, ruleset bump or schema bump are introduced by this stack layer; the base layer already selects Rust ruleset 2.

## Considered options

- **Wait for a third failure since the last pass:** rejected. It misses intermittent ratchets and allows full-load reruns to keep consuming crashes before testing the informative part.
- **Probe after every charge:** rejected as the default. Two is a deliberately simple method threshold, not a fitted optimum: one ordinary backoff remains inexpensive, two repeated charges require an intervention. A disagreeing stall buys the earlier experiment without buying a guilt inference.
- **Count counts of offset movement or compare voltage targets:** rejected. Both make experiment timing depend on estimated requests and crashed measurements; the number of repeated untested charges is the relevant question and works without telemetry.
- **Charge a recurring stalled worker directly:** rejected. Stalls may be consequences of a hang. The experiment can fail naming it, fail naming someone else, or pass; none is preselected by the stall.
- **Idle both CCDs' top groups at once for an all-core failure:** rejected. A separate probe per charged CCD changes less of the load and keeps the other CCD's work present.
- **Never locate a `loaded` load again:** rejected. Passes and changed unloaded offsets supply new grounds to ask the question. Loaded moves alone do not.
- **Treat a passing part as proof that its idled core was guilty:** rejected. Removing load changes shared voltage and boost; five passes do not rule out an intermittent failure.

## Evidence and consequences

`crates/togi-tuner/src/core03-charge-loop.json` contains sanitized trial summaries extracted from the recorded journal: trials 0843–0849, their intended durations, requests and clocks, plus the first recorded partial's named failure (0858). It contains no old tuner decisions, boot identifiers, cursors, paths or host details. `recorded_core03_ratchet_probes_after_two_charges_despite_five_interleaved_passes` folds those observations and applies newly generated decisions in order. It pins the diversion to the partial after −4 → −3 → −2, despite five intervening passes, before another parent rerun can charge core 03 to −1.

The subsequent partial result is explicitly a **counterfactual test input**, not measured hardware evidence at the new profile: recorded trial 0858 had idle core 03 at −1, while the earlier probe leaves it at −2. Its named core-00 result pins the new attribution path without claiming what hardware would have done earlier. The fixture/API is preferable to replaying the complete old journal's decisions, which would merely replay the ratchet already taken. Ordinary boundary tests cover ties, minimum parts, no telemetry, all-core loads, intended duration, named/carried exclusions, retries, reset/resume and repeated loaded locates.

The rule adds up to `n` trials at the failed intended duration per experiment. It can find another loaded failure sooner, but changes the load and cannot prove the cause of the original failure. Suppressing redundant locates can avoid repeated crash experiments; simulator results are reported in the implementation pull request, not treated as proof of hardware behavior.
