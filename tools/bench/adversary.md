# togi adversarial search: find machines the tuner gets wrong

You are an autonomous researcher working on togi, a Go CLI that finds per-core Curve Optimizer offsets on Zen 5 CPUs. Search for simulated machines, consistent with the real evidence, on which the tuner concludes with a profile that still fails, or fails to conclude at all. Each confirmed finding becomes a bench scenario the tuner research must then pass ([research program](program.md)), or a documented limit of the evidence rules. Work in a loop, keep what scores, and explain each finding. Continue until stopped; report and open pull requests for confirmed findings. The owner merges every pull request.

This loop never changes the tuner, the simulator or the bench. It writes only untracked machine and suite files until a finding becomes a pull request.

## The score

A candidate is a simulator machine file, run through the bench with its own suite file:

```toml
[[scenario]]
name = "probe"
machine = "machine.toml"
replay = true
dev = [1, 2, 3, 4, 5, 6, 7, 8]
holdout = [101, 102, 103, 104, 105, 106, 107, 108]
```

```sh
just bench --suite runs/adv/<n>/suite.toml --split dev --out runs/adv/<n>/dev.jsonl --keep runs/adv/<n>/dirs
jq -s '{unsafe: map(select(.status != "concluded" or .hazard_max_per_h >= 1)) | length, max_hazard: (map(.hazard_max_per_h) | max), median_h: (map(.sim_hours) | sort | (.[((length - 1) / 2 | floor)] + .[(length / 2 | floor)]) / 2)}' runs/adv/<n>/dev.jsonl
```

- **Primary:** `unsafe`, the number of the eight dev runs that end without concluding, or conclude with a final profile whose `hazard_max_per_h` is at least 1: the machine's own failure model says that profile fails at least once an hour in some regime with every core loaded. Higher is better. Tie-break by `max_hazard`.
- **Secondary:** a run that takes more than three times the unmodified machine's median `sim_hours` is a cost finding, scored separately.

An unsafe-profile finding must beat the unmodified starting machine by at least three unsafe dev runs. It is confirmed when the holdout split (`--split holdout`) also has at least two unsafe runs and at least half the dev share.

A cost finding needs at least one dev run above the secondary threshold. Confirm it with at least one holdout run taking more than three times the unmodified machine's holdout median `sim_hours`. Score and report cost findings separately; they do not need to increase the unsafe count. Use holdout once per candidate, only to confirm.

## Plausibility

A machine the tuner cannot possibly handle teaches nothing. Every candidate must satisfy all of these:

- **P1, consistent with real evidence.** Start from a committed target fit, `tools/bench/machines/target-fit-<k>.toml`. Keep its `[bios_context]` and point `facts` at `tools/bench/facts/target.jsonl.gz` relative to the new file. The bench's model check must report `ok`: the machine still explains every real group with at least 10 starts. Keep `replay = true`, so trials matching real facts get real answers and the perturbation acts only where the real machine was not measured.
- **P2, existing mechanisms only.** Use the parameters `docs/simulating.md` and `internal/sim/doc.go` describe: per-core, regime and workload edges, idle edges, past-edge, near-edge and flat rates, growth, onset boost, joints, misleading MCEs and reset kinds. Keep rates and edges inside the fitter's ranges in [benchmarking](../../docs/benchmarking.md#fitting-the-target-machine).
- **P3, a physical story.** State in one sentence what about a real CPU the change represents, such as a core whose AVX-512 edge is two counts shallower than its SSE edge, or a CCD that crashes only after ten minutes of heat soak.

A machine built from the seeded default machine instead of a target fit has no P1 evidence. Report such findings separately as unanchored.

## Setup

1. Create a branch `adversary/<tag>` from `main`, with a short descriptive tag. Follow `AGENTS.md` for the repository workflow.
2. Read, in this order:
   - `README.md` and `GLOSSARY.md`, for vocabulary;
   - `docs/spec/tuner.md` and `docs/spec/workloads.md`, for the evidence the tuner requires before it concludes;
   - `docs/simulating.md` and `internal/sim/doc.go`, for the mechanisms you may use;
   - `docs/benchmarking.md`, for the suite, the run records, the model check and the fitter's ranges;
   - `tools/bench/program.md`, for how your scenarios will be used;
   - issues #105, #106, #107 and #114, the findings behind the current hand-written scenarios and the open R4 one.
3. Create `runs/adv/`. Add `runs/`, `results.tsv` and `REPORT.md` to the exclude file located by `git rev-parse --git-path info/exclude`. Create `results.tsv` with the header `n\tbase\tmechanism\tmodel_check\tdev_unsafe\tdev_max_hazard\tdev_median_h\tholdout_unsafe\tverdict\tdescription` (tab-separated).
4. Score every unmodified `target-fit-<k>.toml` as described above. These are the starting points; their unsafe counts are what a finding must beat.

## What is already known

The committed `tools/bench/baseline.jsonl` already has unsafe conclusions. They are not new findings, but explaining them is the cheapest first experiment:

- `default` seeds 21, 104, 106 and 114 conclude one count past a resident R3 or R4 edge with a hazard of 92 failures/h: the guard rotation runs a single start of each, and one start misses about 5% of the time (#107).
- `target` dev seeds 6, 8 and 13 conclude with an R7 hazard of 1.5–1.8/h, and holdout seed 111 with an R2 hazard of 12/h. Their mechanism is not yet explained.
- `misleading-mce` seeds 3 and 103 conclude with an R7 hazard of 3.24/h from its second, unattributed joint. Its first joint deliberately leaves an MCE naming one core (#114), but neither final profile triggers that joint.

## The loop

1. **Hypothesis.** Name the tuner rule you expect to fail, the mechanism that exploits it and the runs it should affect. Example: the guard qualifies R4 with one start, so an R4 edge one count shallower than the other regimes on several cores should make most runs conclude past it.
2. **Build** `runs/adv/<n>/machine.toml` from a starting point with the smallest change that tests the hypothesis, and its `suite.toml` beside it.
3. **Score** the dev split. Require P1's `ok`; a flagged model check disqualifies the candidate.
4. **Climb.** Adjust the mechanism's parameters while P1–P3 hold and the score rises. Change one mechanism at a time.
5. **Confirm** on holdout once.
6. **Minimize.** Revert each parameter change in turn and keep it reverted if the finding survives. The finding is the smallest machine that still scores.
7. **Explain.** Use `just stats --state-dir DIRECTORY` and `go run ./cmd/togi --state-dir DIRECTORY events` on the kept run directories to trace the decisions that let the profile through. Classify the cause:
   - **tuner defect:** the journal held evidence that, under the spec's rules, should have prevented the conclusion;
   - **policy limit:** the spec's evidence rules allow it, as #107's single R4 start does. Quantify the residual risk: the chance a profile this bad passes the evidence the rules require;
   - **beyond evidence:** the hazard is too rare or too late for any trial the configuration runs to see it.
8. **Log** every candidate, scored or rejected, in `results.tsv`.

## Report and pull requests

When stopped, write an untracked `REPORT.md` with each confirmed finding: its machine file, the change from its starting point, P1–P3, dev and holdout scores, the decision trace and its classification. List rejected candidates with their scores and the reason.

For each confirmed tuner defect or policy limit:

- comment the evidence on its existing issue, or file a new one from the bugfix template;
- open a pull request that adds the machine to `tools/bench/machines/`, with `facts = "../facts/target.jsonl.gz"`, adds a scenario with four dev and four holdout seeds to `tools/bench/suite.toml` and its row to the scenario table in `docs/benchmarking.md`, and re-records `tools/bench/baseline.jsonl` with `just bench-baseline` in a separate commit. The new scenario is an evaluation change: the tuner research program's baseline moves with it.

Fixing the tuner is not part of this loop. A fix goes through the tuner research program or an ordinary pull request, so a scenario is never tuned against by the same run that found it.

## Limits

A target-derived machine is checked only where the real machine was measured. Everywhere else, and in every hazard number, it rests on the fitted model, which #306 shows predicts later sessions poorly. A finding shows what the tuner does on a machine consistent with the evidence, not what the real CPU will do. Hazard is the model's steady state with every core loaded, not a measured failure rate.
