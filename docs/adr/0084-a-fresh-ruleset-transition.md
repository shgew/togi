# Start Ruleset 13 fresh, retaining raw daily observations

Status: accepted for this cutover; supersedes [0019](0019-a-ruleset-change-starts-a-seeded-session.md) and [0027](0027-carry-trial-facts.md) for the Ruleset 13 transition.

Per-core search facts and commitments do not establish the new CCD policy's workload coverage. The owner accepted discarding that tuning session ([#1214](https://github.com/shgew/togi/issues/1214)), so [the journal transition](../spec/journal.md#transitions) archives it unchanged, starts fresh and removes `togi-carry` rather than adapting its old policy.

Discarding tuning decisions must not discard observations from ordinary use. Import pre-13 day boots, known profile hours, unclean ends and answers as raw provenance-preserving records; stock observations form the baseline, while nonzero observations retain their actual profiles. The owner's [Q16 decision](https://github.com/shgew/togi/issues/1215) makes the recorded freezes active whole-profile vetoes, not synthetic regime failures. Durable manifests and original-identity deduplication prevent crash-resume from multiplying those observations.

Raw new-policy exposure supports a later ruleset recomputing caps, unlike carrying conclusions. That later transition remains its own design; this ADR neither promises to restart every future climb nor to restore D across rulesets.
