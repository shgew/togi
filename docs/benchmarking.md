# Benchmarking

`tools/bench` runs a fixed suite of simulated sessions and compares two versions of togi. Use it to show that a tuner change makes sessions conclude sooner, or no later, without giving up depth or leaving a profile that fails. Like `tools/sim`, it is a development program and is not shipped.

For the autonomous experiment loop, frozen evaluation environment, proof standard and owner-controlled merge and deployment workflow, follow the [research program](../tools/bench/program.md). [ADR 0029](adr/0029-bench-verdicts-rest-on-fitted-machines.md) records why target claims require checked fits and real-fact replay.

```sh
just bench [--split dev|holdout|all] [--out FILE] [--baseline FILE] [--keep DIR] [--jobs N] [--timeout 180s]
```

## The suite

`tools/bench/suite.toml` lists scenarios. Each scenario names a simulator `machine` file in `tools/bench/machines/` (none means the seeded default machine), or an ensemble in `machines`, and two sets of seeds, `dev` and `holdout`. Ensemble seeds select successive members round-robin by their position within each split, not by the seed value. `replay = true` enables the same-BIOS replay oracle over the selected fitted member:

|Scenario|Models|
|---|---|
|`default`|the seeded default machine|
|`target`|real-fact replay over the all-facts target fit and eight checked bootstrap refits, including CCD0 and CCD1 R7 joints|
|`flat-hazard`|rare failures at any nonzero offset on two cores (issue #105)|
|`late-onset`|R7 failures that start only after four minutes of load|
|`idle-edge`|cores that fail idle at shallower offsets than under load (issue #106)|
|`misleading-mce`|joint crashes that leave an MCE naming one core (issue #114)|

Iterate on `dev`. Run `holdout` only to confirm a result, so the holdout seeds stay unseen by the change being tuned.

`--suite FILE` runs another scenario file instead, with machine paths relative to it. The [adversarial search](../tools/bench/adversary.md) scores candidate machines this way and turns confirmed findings into new scenarios.

The target scenario has 18 dev and 18 holdout seeds: two seeds per ensemble member in each split. Its oracle draws uniformly from decisive facts matching the entire applied profile and trial class (regime, workload, sorted loaded cores and intended duration). Draws are deterministic per seed and trial ID/index. Condition, phase, source ruleset and evidence epoch do not restrict a class match; BIOS context does. The oracle never uses facts from another BIOS context or records without one. Non-matching trials and trial-less failures use the fitted machine. The tuner sees only the resulting journal evidence, not the oracle or its extract.

Every run is a `tools/sim` subprocess with its own state directory, in parallel up to `--jobs`. A run that exceeds `--timeout` of wall time is killed and recorded as `timeout`. `--keep DIR` keeps the state directories under a new `DIR/bench-*/<scenario>/<split>-<seed>` (the path is printed), so the read-only commands can inspect a run with `--state-dir`.

Each simulator subprocess retains parsed events across its simulated reboots and writes `state.json` only at stop, avoiding journal re-parsing and per-event state-file rewrites. It still appends every event to `events.jsonl`; `--keep` leaves the journal and final state available for inspection. This changes wall-clock overhead, not simulated durations or tuner decisions. Recovery and interruption tests use the file-backed path, and fixed-seed equivalence tests compare both output files byte for byte on the default machine and `target-fit-0.toml`.

## What a run records

`--out FILE` writes one JSON object per run:

- `status`: `concluded` (the session stopped after its clean qualifying rotation), `deadend`, `error` or `timeout`;
- `sim_hours`: simulated time from `session.start` to the last event, including the 90 s each crash reboot costs. This is the time to conclusion;
- `first_clean_rotation_h`: simulated time to the first clean qualifying rotation;
- `crashes`, `trials`, `trial_hours`, `hunts` and `joint_marks`;
- `real_answers` and `real_answer_share`: the number and fraction of completed trials answered by matching real facts; inconclusive trials and failures before workload startup count in `trials` but not as real answers;
- `scenario_real_answer_share`: real answers divided by completed trials across the scenario's selected runs, not an average of per-run fractions;
- `machine`: the fitted ensemble member selected for the seed.
- `final_profile` and `depth`, its sum;
- `hazard_per_h` and `hazard_max_per_h`: failures per hour at the final profile with every core loaded, per regime, from the simulator's own failure model. A profile that passed by luck shows up here, and no journal can show it.

The commit, a dirty flag and the ruleset are recorded with every run.

The summary prints each scenario's `real_answer_share` and a pooled total. Comparison scenario rows and the verdict line print the candidate and baseline shares over paired runs. These fractions describe how much of the observed path has direct real evidence, not a confidence score. Unfinished trials in a timed-out subprocess have no recorded outcome and are not counted. Hazard metrics and model checks still describe the fitted fallback, not an empirical oracle hazard.

## Comparing two versions

Run the base version with `--out base.jsonl` and the candidate with `--baseline base.jsonl`. Runs pair by scenario and seed; a warning counts candidate runs without a base run, and base runs in the candidate's splits without a candidate run. For each pair the ratio is candidate `sim_hours` over base `sim_hours`. Each scenario reports the geometric mean of its ratios with a 95% bootstrap interval. The overall ratio weights scenarios equally, and its interval resamples pairs within each scenario. The last line is a verdict:

- `REJECT`: any violation, or the interval's lower bound above 1.0;
- `ACCEPT`: no violation and the interval's upper bound below 1.0;
- `NEUTRAL`: anything else.

Violations:

- **V1:** a run the base concluded no longer concludes.
- **V2:** a run's `hazard_max_per_h` rises by more than 0.01.
- **V3:** depth gets shallower: by more than 1 count averaged over a scenario, or by more than 5 in one run.
- **V4:** `target`, the target machine's replay-oracle ensemble, gets slower overall.

Seeds are deterministic: the same commit always produces the same runs, so rerunning cannot change a result. A change to how the tuner decides moves later sessions onto different random paths, so compare whole scenarios, not single seeds.

Record the baseline again whenever the tuner on `main`, the suite, the facts or the fitted machines change:

```sh
just bench --split all --out tools/bench/baseline.jsonl
# Equivalent recipe:
just bench-baseline
```

The recorded run lives at `tools/bench/baseline.jsonl`, the path the research program compares against. It includes both dev and holdout seeds. Baselines recorded before the oracle ensemble do not cover `target`; do not reuse them for V4.

Compare a candidate with the recorded baseline:

```sh
just bench --baseline tools/bench/baseline.jsonl
just bench --split holdout --baseline tools/bench/baseline.jsonl
```

## Checking a machine against real evidence

A machine file may declare `facts = "../facts/target.jsonl.gz"`, resolved relative to the machine TOML. Every `target-fit-*.toml` declares the committed target-machine extract. Files without `facts` are not checked.

Regenerate the extract from a temporary copy of the state directory, never the live state:

```sh
just facts COPY-OF-STATE-DIR
```

The generator reads every archived and live session through `internal/facts`, including sessions before `reset --all` and older builds. Its gzip JSON Lines output is byte-stable for fixed inputs. Generation rejects sources with no facts and writes to a temporary file beside the destination; the committed extract is replaced only after extraction and file close succeed, so an unreadable or empty source leaves it unchanged. Records retain session, build, ruleset, BIOS context, sequence, trial ID, kind, class, condition, phase, full profile, outcome, signal and measured duration. They omit journal timestamps, paths, hostnames and boot IDs. `tools/trialfacts` is the shared reader for evaluation tools.

The model check groups decisive starts by BIOS context, trial class (regime, workload, sorted loaded cores and intended duration), and the shallowest loaded-core offset in the applied profile. Any loaded core at 0 puts the start at depth 0. If the machine declares a BIOS context, only matching facts are checked; otherwise contexts are checked separately.

This check uses decisive pass/failure starts only; interrupted and inconclusive trials are omitted. A facts-declaring machine with positive `thermal_trip` or `power_loss` weights in `[model.reset]` is rejected: those resets can turn simulated crashes into inconclusive outcomes, while the simulator's failure probability includes the full failure hazard. Comparing that probability to decisive-only starts would use incompatible denominators.

For each group with at least 10 starts, the simulator computes each start's exact failure probability over its intended duration, including onset boosts, unloaded-core hazards and delayed joints. The check averages these probabilities and compares observed failures against quantiles 0.005 and 0.995 of Binomial(n, mean_p). A count outside that inclusive interval flags the machine. Idle failures are retained as their own fact class but have no start duration or exposure denominator, so the report counts them separately rather than inventing a per-start prediction.

`just bench` prints `ok` or `flagged` for each checked file, and every offending class, depth, n, k, interval and mean_p. JSON run records carry the same information in `model_check`, including all eligible groups. A flag says the file does not explain that part of the real evidence; it does not change the comparison verdict or exit status. Investigate the group before using improvements on that file as evidence about the target machine. This is a model diagnostic, not a claim that adaptive journal starts are independent or that all groups jointly have a 99% coverage guarantee.

## Fitting the target machine

```sh
just fit
# Optional: --facts EXTRACT.jsonl.gz --out DIRECTORY --seed 263 --bootstrap 8
```

`tools/fit` reads the same privacy-safe extract as the model check. It writes `target-fit-0.toml` from all decisive starts and `target-fit-1.toml` through `target-fit-8.toml` from whole-trial bootstrap samples drawn with replacement. The default seed is 263; refit `n` uses seed `263+n`. Every file declares its extract relative to the output directory and its BIOS context. Mixed-context extracts are refused: split them before fitting. The `target` scenario spreads separate dev and holdout seeds across all nine files, with the replay oracle above each.

Each bootstrap sample is fitted without constraints first. If its model check against the original extract flags a group, the fitter restarts from the passing all-facts fit and maximizes that **same resampled likelihood**, accepting only parameter moves that remain inside every original group's unchanged 99% interval. It does not redraw the sample, tune the seed, change the interval or repair the observed counts. This is a **checked, constrained bootstrap ensemble**: its spread is truncated to what the real facts admit, not an unconstrained bootstrap confidence interval. The fit output identifies each constrained refit and its flagged groups, including the unconstrained interval and mean probability. Each generated header records the bootstrap index and constrained groups' class, depth and observed counts; the usual final model-check report still checks the serialized machine files.

For the committed extract at seed 263, refits **2 and 8** required the constraint on `R7 mprime-avx2-36k-248k-allcore`, cores 0–7, 120 s, depth -24 (`n=45`, `k=5`). Their unconstrained predictions were respectively `mean_p=0.0178977`, interval `[0,4]`, and `mean_p=0.0125439`, interval `[0,3]`. The committed all-facts fit and all eight constrained bootstrap members report `ok` on all 44 eligible groups. Data generation took about **2 min 15 s** in a development run; allow a few minutes for `just fit`, including development-shell startup and Go compilation.

The objective is the Bernoulli negative log likelihood of observed passes and failures, using `sim.Machine.FailureProbability` over each start's **intended**, not measured, duration. Equal profile/class observations are aggregated without losing their counts. This includes unloaded-core failures and uses exactly the prediction the model check uses. A bounded coordinate search fits integer per-core isolated/resident regime edges, the shared past-edge rate and growth, the shared near-edge rate, per-core flat rates and unloaded-core idle edges. R7 failures support layered CCD shared-rail joints: each CCD starts with one all-member joint, and additional failure-profile candidates are accepted when they improve negative log likelihood by at least 0.5, up to eight layers per CCD. Joint member thresholds are fitted independently, not forced to a common depth. Workload edge overrides require at least 10 loaded starts, a failure, and the same minimum likelihood improvement. Search stops after twelve sweeps or negligible likelihood improvement; it is a local maximum-likelihood fit conditional on the selected structure, not a guarantee of a global optimum.

Each sweep also searches coupled integer edge/rate shifts. Moving active per-core, workload and idle edges by `delta` and scaling the past-edge rate by `growth^(-delta)` preserves already-past-edge hazards until a profile crosses an edge. This lets the search remove false hazards at observed clean boundaries instead of getting stuck when separate edge and rate moves each worsen the likelihood. Unsupported -50 edges stay fixed; joints are not shifted, and the same rate/edge bounds and original-evidence constraints still apply.

The positive-rate search covers past-edge rates from `1e-8` to `0.5` failures/s, near-edge rates from `1e-8` to `0.001`/s, flat rates from `1e-10` to `0.001`/s, and joint rates from `1e-7` to `0.5`/s. Growth ranges from `1.05` to `8`. Zero is also considered for past-edge, near-edge and flat rates; the joint search's zero candidate is clamped to `1e-12`/s because the simulator interprets a literal zero joint rate as the default past-edge rate. Edges range from -50 to 1, matching the simulator's stock-failure representation.


The fitter opts R1/R2 singleton starts into the smooth `[single_core]` hazard described in [simulating](simulating.md). It shares the offset slope and workload effects across cores, and uses one loaded-core rule for isolated and resident profiles. Core and workload log-rate effects have unit-normal shrinkage toward zero; the shared log rate is fitted without this penalty. Log rate is bounded to [−20, −2], slope to [0, 1], and effects to [−5, 5]. Unsupported core effects remain zero; a previously unseen workload uses the shared mean. Fitted likelihood output includes the shrinkage penalty. Other starts retain the edge, idle and joint search above.
The extract cannot identify every simulator parameter. Unsupported or all-passing edge boundaries stay at -50; absence of failures does not prove that boundary. The fit shares the hazard shape across cores and regimes because sparse failures cannot resolve a separate shape for each. Unsupported workload overrides stay absent; flat rates stay zero without supporting failures. Onset boost and joint delays stay at zero: decisive binary starts do not identify failure-time shapes. Signal weights, crash-MCE probability, bank attribution and reset kinds retain simulator defaults; these are not parameters of the binary likelihood. Idle facts without starts are counted by the check but provide no idle exposure denominator. Idle edges can be fitted only through decisive starts with nonzero offsets on unloaded cores; otherwise they stay disabled. Offset-zero unloaded starts also constrain idle-edge candidates, since an edge of 1 adds a hazard at stock offsets. Failure-only joint activation sets identify a lower bound, not an upper bound on the rate; a saturated fitted rate is a deterministic representative on that likelihood plateau, not a precise physical measurement. Generated headers summarize the fitted parameter families, unsupported-edge and unobserved-idle limits, and the fixed onset, delay, signal, MCE and reset settings.

Generation prints likelihoods, CCD joint parameters, model checks against the **original** extract (also for bootstrap fits), and elapsed wall time. Files contain no timestamps and use stable ordering and full-precision parameters, so fixed inputs and seed reproduce them byte for byte. A model-check flag is not silently repaired or excluded: inspect the named class, depth and interval before using that ensemble member as target evidence. A flag may reflect a poor local optimum, the current model's shared-shape/joint assumptions, or a sparse failure absent from a bootstrap sample; it is not by itself proof of an impossible fit. The shared `tools/modelcheck` implementation is used by both `just fit` and `just bench`, with the same 99% intervals.

### Forward-chained check

After the ensemble's model check, `just fit` orders sessions with decisive starts by their UTC session IDs, comparing equal-second numeric suffixes numerically (`-2` before `-10`). For each session after the first, it fits all earlier sessions' decisive starts once, without bootstrap resampling or model-check constraints, then predicts only the held-out session. These fits stay in memory and do not change the generated machine files.

Each session row names the session ID, ruleset and `training_sessions` count. `starts` and `failures` count held-out decisive starts and observed failures; `predicted` sums the fit's failure probabilities over their intended durations. `log_loss/start` is the mean Bernoulli log loss, with probabilities clamped to `[1e-4, 1-1e-4]`. It compares the `fit` to a `constant` predictor whose probability, shown in parentheses, is the earlier training starts' failure rate. Lower loss is better: fit loss above the constant means the fit predicts that later session worse than a single average failure rate.

`failures_p<0.01` counts observed failures assigned less than 1% probability: outcomes the model treated as nearly impossible. `exact_matches` is the number and share of held-out starts whose trial class and full profile occur in the earlier training starts. That share bounds what replay could answer from earlier evidence; it does not establish that replay's answers would be correct. `flagged/eligible` counts held-out groups flagged by the unchanged model check, with at least 10 starts and the same 99% binomial intervals. The indented regime rows break down held-out starts and observed versus predicted failures.

`Pooled` sums all held-out sessions' counts and predictions and divides summed log losses by their total starts. Its constant uses each session's own earlier prefix, not one failure rate fitted to the pooled outcomes; its group counts sum the separate held-out checks. `Forward-chained elapsed` reports the added wall time. This is evidence about extrapolation to later sessions, not a gate or a guarantee about unseen profiles or other machines.

```sh
just forward            # the check alone: no ensemble, no machine files
just forward --seal 1   # also leave the newest session unfitted and unscored
```

`just forward` runs `tools/fit --forward-only`: the same rows and `Pooled` line, without fitting the ensemble or writing machine files. `--seal N`, accepted only with `--forward-only`, drops the newest N sessions before the check: they are neither fitted nor scored, the remaining rows are unchanged, and `Pooled` covers only the remaining held-out sessions. At least one held-out session must remain. Model research ([program](../tools/fit/program.md)) iterates with the newest session sealed and scores it only to confirm a kept change.

The committed extract produced these rows with the default `just fit` arguments:

```text
Forward-chained check
20260926T151414Z ruleset=2 training_sessions=1: starts=479 failures=22 predicted=2.5 log_loss/start fit=0.3478 constant(0.064)=0.1892 failures_p<0.01=20 exact_matches=44/479 (9.2%) flagged/eligible=0/0
  R1 starts=195 observed=1 predicted=0.4
  R2 starts=155 observed=17 predicted=0.3
  R3 starts=56 observed=0 predicted=0.1
  R4 starts=50 observed=0 predicted=0.1
  R5 starts=18 observed=2 predicted=0.0
  R6 starts=3 observed=0 predicted=0.1
  R7 starts=2 observed=2 predicted=1.6
20260927T221954Z ruleset=3 training_sessions=2: starts=403 failures=18 predicted=9.0 log_loss/start fit=0.1841 constant(0.057)=0.1840 failures_p<0.01=10 exact_matches=89/403 (22.1%) flagged/eligible=0/0
  R1 starts=51 observed=0 predicted=0.0
  R2 starts=281 observed=4 predicted=0.4
  R3 starts=17 observed=0 predicted=0.0
  R4 starts=17 observed=0 predicted=0.0
  R5 starts=17 observed=1 predicted=0.0
  R7 starts=20 observed=13 predicted=8.6
20260929T180308Z ruleset=4 training_sessions=3: starts=894 failures=59 predicted=13.9 log_loss/start fit=0.4597 constant(0.054)=0.2445 failures_p<0.01=44 exact_matches=10/894 (1.1%) flagged/eligible=6/8
  R1 starts=80 observed=0 predicted=0.0
  R2 starts=80 observed=0 predicted=0.0
  R7 starts=734 observed=59 predicted=13.9
20261002T004254Z ruleset=6 training_sessions=4: starts=317 failures=8 predicted=18.9 log_loss/start fit=0.0666 constant(0.058)=0.1302 failures_p<0.01=0 exact_matches=246/317 (77.6%) flagged/eligible=1/4
  R1 starts=80 observed=0 predicted=0.0
  R2 starts=80 observed=0 predicted=0.0
  R7 starts=157 observed=8 predicted=18.9
Pooled: starts=2093 failures=107 predicted=44.4 log_loss/start fit=0.3215 constant(per-prefix)=0.2029 failures_p<0.01=74 exact_matches=389/2093 (18.6%) flagged/eligible=7/12
```

In one development run, the four added prefix fits and held-out checks took about 43 s. This is an observed wall time, not a runtime benchmark.
