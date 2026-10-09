# togi tuner research: shorten the time to a conclusion

Rules for an autonomous researcher making a tuning session reach its conclusion in less simulated time without making that conclusion worse. The loop is experiments: keep what wins, discard what does not. Agents open pull requests under `AGENTS.md`; target-machine deployment stays with the owner.

A session concludes when every core is at its limit, deepening can reach no more total depth, and a clean cycle supports the resulting profile. Time to conclusion is simulated time from `session.start` to the last event, including the 90 s charged for each crash reboot. A full cycle records tested coverage, not a guarantee of future stability.

## Start

Branch from `main` and follow `AGENTS.md`. Read `docs/spec/tuner.md`, `docs/spec/journal.md`, ADRs 0020, 0023, 0024, 0027, 0028 and 0029, `docs/prior-art.md` and `docs/benchmarking.md`. Confirm `tools/bench/baseline.jsonl` covers both splits of the current suite and describes the unchanged tuner and environment; if it is missing or stale, report that prerequisite rather than recording an experiment as the baseline. Before experiments, `just bench --split dev --baseline tools/bench/baseline.jsonl` on the unchanged branch must show ratios of 1.000, no violations and complete pairing, and its flagged target members, which block target-machine claims ([ADR 0044](../../docs/adr/0044-write-flagged-target-fits.md)), are noted.

## What you may change

- `internal/tuner/**`: search, hunts, deepening and checking decisions.
- Tests and specifications describing a deliberately changed rule.

Everything else is frozen for tuner experiments:

- **The environment:** `internal/sim` and `tools/bench`, including the suite, facts, fitted machines and baseline; `tools/facts`, `tools/fit`, `tools/modelcheck` and `tools/trialfacts`.
- **The run loop and journal:** `internal/session`, `internal/journal` and the journal schema.
- **The hardware path:** `internal/smu`, `internal/trial` and `internal/detect`.
- **Configuration defaults and the evidence they ask for** (**Evidence volume** below). Trading evidence for time is safe only when the simulator predicts failures at depths the journal has not tested, and the forward-chained check in `just fit` ([benchmarking](../../docs/benchmarking.md#forward-chained-check)) shows it does not yet. The bench's hazard comes from that same simulator, so it cannot catch a shorter or smaller evidence set that misses a real failure. The owner lifts this freeze in a separate pull request, once the forward-chained check supports it.

A simulator or machine-file change is an evaluation change, not a tuner experiment; record ideas needing frozen files for an owner decision. Refreshing evidence with `just facts` and `just fit` is an owner-triggered evaluation update: review its model checks and record a new baseline before comparing tuners in the changed environment. Use a copy of the state directory, never the live one.

## Hard rules

Breaking any rule invalidates an experiment, however good its ratio looks.

- **Evidence only.** The tuner decides from journal evidence. It never reads simulator internals, fitted machines, facts extracts, seeds or scenario identities. Never special-case a core number, CCD or scenario's offsets; topology recorded in the journal can inform a tested hypothesis.
- **Traceable and deterministic.** Every new decision is a journal event with a cause and a plain-language `msg`, as the journal spec requires. Replaying the journal gives the same decisions.
- **Safe writes.** Offsets stay clamped to [-50, 0] and pass failure point or combination validation, including intermediate SMU writes.
- **Failure detection stays intact.** Failures remain evidence under the spec's coverage and supersession rules. Never ignore, retry away or reclassify a failure to save time.
- **Evidence volume.** Preserve the spec's trial requirements: one R1 and one R2 trial for an ordinary search step; the decisive trials set by `[evidence]` miss and rate for candidate-solo-limit checks, hunt groups, deepening checks and rerun obligations. Each trial runs for its configured duration from `[durations]`, and a full cycle runs every regime trial in `cycle = [...]` in `[checking]`. A change may reorder these trials, skip work the spec already allows to be skipped, or avoid redundant trials. It may not ask for fewer trials, shorter trials or a smaller cycle, whether through configuration or through tuner code.
- **Tests pass.** Run the affected tests before each candidate bench run, and `just gate` before keeping a commit. Update tests for deliberately changed rules; remove tests that only pin obsolete implementation behavior.
- **Proof has three parts.** A kept change wins the bench, preserves every guardrail and bases target-machine claims on fitted files that pass the model check against real facts. The harness prints model flags but does not reject the verdict or change exit status for them: inspect them separately. A flagged target member blocks the target claim even if the verdict says ACCEPT.
- **No ruleset bump per experiment.** One bump covers the kept decision changes when they become pull requests; file each as a sub-issue of the open `Ruleset N` issue (`docs/issues.md`) so they share one bump, one bench gate and one baseline.

## Method

[Benchmarking](../../docs/benchmarking.md#comparing-two-versions) defines pairing, the comparison ratio, bootstrap interval, the V1–V4 guardrails and verdicts; check printed verdicts and violations, not just exit status. The `target` scenario's replay oracle draws a real outcome when the whole applied profile and trial class match same-BIOS facts and otherwise uses the fitted member, so report each verdict's `real_answer_share`: it measures direct real-fact coverage, not confidence.

- State one falsifiable hypothesis per experiment, and bound the mechanism's possible saving on kept run directories (`just stats --state-dir DIRECTORY`) before building it.
- Compare each candidate against the current best on the dev split. Seeds are deterministic; rerunning the same commit cannot improve its draw. A timeout is a failure to conclude, not a result to exclude.
- Use the holdout split once per dev winner, only for confirmation. Discard NEUTRAL, REJECT and failed confirmations.
- Explain a win by checking that the predicted quantity moved. Ablate kept changes periodically and drop any that no longer contribute.
- Record every experiment, kept or rejected, with its numbers.
- Compare the final combination with the baseline on both splits, and state the measured proof and its limits in each pull request.

ADR 0023 rejected probing the shallowest member first, always backing off the shallowest member, moving every member to its passing probe, and reusing member-probe evidence across hunts. Revisit these only with a new argument.

The simulator is a model, not the machine. Prefer mechanisms grounded in evidence rules over constants fitted to the simulator, keep the adversarial scenarios as checks beyond the target fit, and scope claims to the measured suite and real-answer coverage. Bench evidence does not authorize a hardware deployment.
