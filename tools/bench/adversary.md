# togi adversarial search: find machines the tuner gets wrong

Rules for an autonomous researcher searching for simulated machines, consistent with the real evidence, on which the tuner concludes with a profile that still fails, or fails to conclude at all. Each confirmed finding becomes a bench scenario the tuner research must then pass ([research program](program.md)), or a documented limit of the evidence rules.

This loop never changes the tuner, the simulator or the bench. It writes only untracked machine and suite files until a finding becomes a pull request. Its entry condition is a passing target fit: while every committed fit is flagged, there is no plausible starting point.

## The score

A candidate is a simulator machine JSON file run through the bench with its own JSON suite file (`just bench --suite FILE`, machine paths relative to it) with `"replay": true` on its scenario and eight dev and eight holdout seeds.

- **Primary:** `unsafe`, the number of the eight dev runs that end without concluding, or conclude with a final profile whose `hazard_max_per_h` is at least 1: the machine's own failure model says that profile fails at least once an hour in some regime with every core loaded. Higher is better. Tie-break by the largest `hazard_max_per_h`.
- **Secondary:** a run that takes more than three times the unmodified machine's median `sim_hours` is a cost finding, scored separately.

An unsafe-profile finding must beat the unmodified starting machine by at least three unsafe dev runs. It is confirmed when the holdout split also has at least two unsafe runs and at least half the dev share. A cost finding needs at least one dev run above the secondary threshold and at least one holdout run taking more than three times the unmodified machine's holdout median. Use holdout once per candidate, only to confirm.

## Plausibility

A machine the tuner cannot possibly handle teaches nothing. Every candidate must satisfy all of these:

- **P1, consistent with real evidence.** Start from a committed target fit, `tools/bench/machines/target-fit-<k>.json`. Keep its `bios_context` object and point `facts` at `tools/bench/facts/target.jsonl.gz` relative to the new file. Preserve the fit's provenance and qualifications in its `notes` array of strings. The bench's model check must report `ok`: the machine still explains every real group with at least 10 trials. A flagged model check disqualifies the candidate. Keep `"replay": true` on the suite scenario, so trials matching real facts get real answers and the perturbation acts only where the real machine was not measured.
- **P2, existing mechanisms only.** Use the parameters `docs/simulating.md` and `internal/sim/doc.go` describe. Keep rates and limits inside the fitter's ranges in [benchmarking](../../docs/benchmarking.md#fitting-the-target-machine).
- **P3, a physical story.** State in the machine's `description` string what about a real CPU the change represents, such as a core whose AVX-512 limit is two counts shallower than its SSE limit.

A machine built from the seeded default machine instead of a target fit has no P1 evidence. Report such findings separately as unanchored.

## Method

State the tuner rule you expect to fail and the mechanism that exploits it. Change one mechanism at a time while P1–P3 hold and the score rises, confirm on holdout once, then minimize: revert each parameter change in turn and keep it reverted if the finding survives. Explain the finding with `just stats --state-dir DIRECTORY` and `togi events` on the kept run directories, and classify its cause:

- **tuner defect:** the journal held evidence that, under the spec's rules, should have prevented the conclusion;
- **policy limit:** the spec's evidence rules allow it. Quantify the residual risk: the chance a profile this bad passes the evidence the rules require;
- **beyond evidence:** the hazard is too rare or too late for any trial the configuration runs to see it.

For each confirmed tuner defect or policy limit, comment the evidence on its existing issue or file one from the bugfix template, and open a pull request that adds the machine to `tools/bench/machines/` with its scenario and re-records `tools/bench/baseline.jsonl` in a separate commit; the new scenario is an evaluation change, so the tuner research baseline moves with it. Fixing the tuner is not part of this loop, so a scenario is never tuned against by the run that found it.

## Limits

A target-derived machine is checked only where the real machine was measured. Everywhere else, and in every hazard number, it rests on the fitted model, which predicts later sessions poorly ([#306](https://github.com/shgew/togi/issues/306)). A finding shows what the tuner does on a machine consistent with the evidence, not what the real CPU will do. Hazard is the model's steady state with every core loaded, not a measured failure rate.
