# togi autoresearch: shorten the time to a conclusion

You are an autonomous researcher working on togi, a Go CLI that finds per-core Curve Optimizer offsets on Zen 5 CPUs. Your job is to make a tuning session reach its conclusion in less simulated time without making that conclusion worse. Work in a loop of experiments, keep what wins, and discard what doesn't. Do not stop to ask whether to continue; run until you are stopped.

## What "conclusion" means

A session concludes when every core is done, refinement can reach no more depth, and one clean qualifying guard rotation has passed on the resulting profile. `tools/sim` stops a simulated session at that point and exits 0. The time to conclusion is the simulated time from `session.start` to the last event. It counts trial time and the 90 s the simulator charges for each crash reboot.

## Setup

1. Create a branch `autoresearch/<tag>` from `autoresearch`, with a short tag such as the date.
2. Read, in this order:
   - `README.md` and `CONTEXT.md`, for the vocabulary;
   - `docs/spec/tuner.md`, the normative decision rules;
   - `docs/adr/0020-hunt-and-refine.md` and `docs/adr/0023-hunts-that-converge-on-shared-voltage.md`, the last two decisions and the alternatives they rejected;
   - `docs/prior-art.md`;
   - `docs/simulating.md` and `internal/sim/doc.go`, the failure model;
   - `tools/bench/` and its `suite.toml` and `machines/`, the harness.
3. Run `just bench --split dev --baseline tools/bench/baseline.jsonl --out runs/0000.jsonl` on the unchanged branch. Every scenario's ratio must be 1.000 with no violations. If not, stop and report it: the harness is broken. Copy `runs/0000.jsonl` to `runs/best.jsonl`. Then run `just bench --split holdout --out runs/best-holdout.jsonl`.
4. Add `runs/`, `results.tsv` and `REPORT.md` to `.git/info/exclude`, and create `results.tsv` with the header `commit	dev_ratio	dev_ci	holdout_ratio	crashes_delta	depth_delta	hazard_delta	verdict	description`.

A full dev run takes about one to two minutes on 16 cores. A candidate that makes sessions pathologically slow hits the 180 s per-run timeout and counts as not concluding.

## What you may change

- `internal/tuner/**`: the decision engine, meaning search, hunts, refinement, guard and tiers.
- The defaults in `internal/config` that set trial durations and start counts.

Everything else is frozen. In particular:
- **The environment:** `internal/sim` and `tools/bench`, including the suite, the machines and the baseline.
- **The run loop and journal:** `internal/session`, `internal/journal` and the journal schema.
- **The hardware path:** `internal/smu`, `internal/trial` and `internal/detect`.

If an idea needs a frozen file changed, write it down in the final report instead of making the change.

## Hard rules

Breaking any of these invalidates the experiment, however good the number looks.

- **Evidence only.** The tuner decides only from journal evidence. It never reads simulator internals, the machine file, the seed, or anything that identifies a scenario. Never special-case a core number, a CCD, or a scenario's offsets.
- **Traceable and deterministic.** Every new decision is a journal event with a cause and a plain-language `msg`, as `docs/spec/journal.md` requires. Replaying the journal gives the same decisions.
- **Safe writes.** Offsets stay clamped to [-50, 0] and pass mark validation, intermediate SMU writes included.
- **Failure detection stays intact.** A failed mark is permanent within a session. A failure is never ignored, retried away, or reclassified to save time.
- **Tests pass.** `go test ./internal/tuner/ ./internal/simrun/ ./cmd/togi/` passes before every bench run. Update tests whose rule you deliberately changed; delete tests that pinned the old behavior. Run `just gate` before you keep a commit.
- **No ruleset bump per experiment.** One ruleset bump, made when the work becomes pull requests, covers every kept change.

## The metric

`just bench` runs every scenario and seed of a split in parallel. It writes one JSON line per run and, with `--baseline`, prints a verdict line:

```
verdict: ACCEPT|REJECT|NEUTRAL ratio=<geo-mean> ci=[lo,hi] ...
```

- **Primary, `ratio`:** the candidate's time to conclusion divided by the baseline's. Runs are paired by scenario and seed, the geometric mean is taken within each scenario, and the scenarios are averaged with equal weight. The CI is a stratified bootstrap. Lower is better.
- **Guardrails:** the harness rejects a candidate that breaks any of these, whatever its ratio.
  - **V1:** a run that concluded on the baseline no longer concludes (dead end, error or timeout).
  - **V2:** the final profile's steady-state failure rate (`hazard_max_per_h`, computed from the simulator's true model) rises by more than 0.01 per hour in any run. This catches profiles that passed by luck.
  - **V3:** the final profile gets shallower: by more than 1 count on average in a scenario, or by more than 5 in any run.
  - **V4:** the `shared-rail` scenario, the target machine's shape, gets slower.
- **Secondary:** crashes, trials and hunts. At equal time, prefer fewer crashes: on real hardware every crash is a reset with about 65 s of recovery and some risk.

The verdict is ACCEPT when the CI's upper bound is below 1.0 and nothing is violated. It is REJECT on any violation or when the CI's lower bound is above 1.0. Otherwise it is NEUTRAL.

## The loop

1. **Hypothesis.** Write one falsifiable sentence: the mechanism, the scenarios it should help, and the expected size. For example: "Guard runs R1 to R6 before R7, so after a profile change on shared-rail, R7 fails only after about N hours of other regimes. Starting a rotation with the regime that failed last should cut shared-rail time by at least 10% and leave default unchanged."
2. **Bound it before building it.** Run `just bench --split dev --baseline runs/best.jsonl --keep runs/dirs` on the current best, then `just stats --state-dir runs/dirs/<run>` on a few runs. Measure the most time the mechanism could save. If that bound is under 3% of the scenario's time, pick another idea.
3. **Implement the smallest change** that tests the hypothesis, in one commit.
4. **Run the dev split** against the current best: `just bench --split dev --baseline runs/best.jsonl --out runs/<n>.jsonl`. Seeds are deterministic: a rerun of the same commit gives identical results, so never rerun to get a better draw.
5. **Check the mechanism, not just the outcome.** Look at whether the quantity you predicted moved, using `just stats` on a kept run. A win with an unexplained mechanism is suspect; look for a loophole in your change before keeping it.
6. **Decide.**
   - **ACCEPT on dev:** run `just bench --split holdout --baseline runs/best-holdout.jsonl --out runs/<n>-holdout.jsonl`. Keep the commit only if the holdout shows no violation and a ratio below 1.0. Then copy the two outputs to `runs/best.jsonl` and `runs/best-holdout.jsonl`. Run the holdout only for dev winners, once each, so it stays a holdout.
   - **NEUTRAL or REJECT:** `git reset --hard HEAD~1`. A NEUTRAL change that removes code without slowing anything may be kept; note it in the log.
7. **Log** one row in `results.tsv` for every experiment, kept or not.
8. **Every 5 kept commits,** ablate: revert each kept change alone and re-run dev, to confirm each one still contributes. Drop any that no longer do.

When an experiment errors, read the sim log in the kept run dir (`--keep`), fix the bug if it is trivial, and otherwise log it as a crash and move on. A run that hits the harness timeout counts as failing to conclude.

## Where the time goes

Measure the current split yourself before trusting this. The committed baseline, ruleset 5:

|scenario|runs|median h|mean h|median crashes|mean depth|
|---|---:|---:|---:|---:|---:|
|default|48|20.1|20.2|13.5|-301.8|
|shared-rail|16|41.2|43.0|37|-669.0|
|flat-hazard|8|41.5|65.7|35.5|-657.9|
|late-onset|8|28.6|28.8|74|-668.0|
|idle-edge|8|23.6|23.6|104|-662.0|
|misleading-mce|8|65.9|64.3|78.5|-656.6|

- **Crash reboots.** idle-edge and late-onset spend more crashes than any other scenario, and misleading-mce is the slowest. On shared-rail most of the 37 crashes come from hunts over CCD0, whose 8 cores fail together when the shallowest of them is deep.
- **Hunts.** A hunt is a group test over masks of cores, and each mask needs several clean starts. Fewer masks per hunt, better first splits (`session.start` records each core's CCD), and stopping once the evidence settles the outcome all cut time.
- **Guard.** A qualifying rotation runs every regime, so a failure late in the rotation wastes the earlier regimes' time.
- **Search.** Isolated search steps every core down from its candidate edge. Start counts and step sizes set how long it takes.
- **Durations.** Shorter trials cost less per start but miss late-onset hazards. The `late-onset` scenario and V2 exist to catch that.
- **A safety defect you may fix.** On dev seed 21 and three holdout seeds of `default`, the session concludes with one core a count past its R3 or R4 edge. Those profiles fail at about 92 per hour (issue #107: a single R4 guard start can certify an offset past the edge). V2 flags only increases, so lowering this is a real win; report it separately.

Ideas worth testing, none proven:
- sequential stopping, which ends a mask or probe early once its outcome is statistically settled;
- regime order in guard;
- mask design that uses CCD topology as a prior while still testing it;
- more evidence reuse where the spec's inference rules allow it;
- fewer redundant probes in a joint backoff;
- step sizes in search and refinement.

Already tried and rejected, so don't repeat them without a new argument (ADR 0023):
- probing the shallowest member first;
- backing off the shallowest member as a rule;
- moving every member to its passing probe;
- reusing edge-probe evidence across hunts.

## Why the simulator can mislead

The simulator is a model, not the machine. Prefer changes that follow from the evidence rules over changes that tune constants to the model:
- **Failure rates:** a core one count past its edge fails a 90 s trial 90% of the time, and each further count quadruples the rate. Real hardware is noisier.
- **Determinism:** draws are keyed by core, regime, offset and trial index, so a retried trial sees the same outcome.

A change whose gain disappears on `flat-hazard`, `late-onset`, `idle-edge` or `misleading-mce` is probably fitting the model.

## Final report

When stopped, write `REPORT.md` (untracked) covering:
- the kept commits in order, each with its hypothesis, mechanism, dev and holdout ratios, and the effect on crashes and depth;
- the ablation results;
- the ideas tried and rejected, with numbers;
- ideas that need frozen files changed;
- which spec sections and ADRs each kept change would need edited.

That report is what turns the kept commits into pull requests.
