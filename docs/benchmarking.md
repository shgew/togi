# Benchmarking

`tools/bench` runs a fixed suite of simulated sessions and compares two versions of togi. Use it to show that a tuner change makes sessions conclude sooner, or no later, without giving up depth or leaving a profile that fails. Like `tools/sim`, it is a development program and is not shipped.

```sh
just bench [--split dev|holdout|all] [--out FILE] [--baseline FILE] [--keep DIR] [--jobs N] [--timeout 180s]
```

## The suite

`tools/bench/suite.toml` lists scenarios. Each scenario names a simulator machine file in `tools/bench/machines/` (none means the seeded default machine) and two sets of seeds, `dev` and `holdout`:

|Scenario|Models|
|---|---|
|`default`|the seeded default machine|
|`shared-rail`|the target machine's first ruleset-4 run: CCD0's eight cores crash R7 together when the shallowest is deep|
|`flat-hazard`|rare failures at any nonzero offset on two cores (issue #105)|
|`late-onset`|R7 failures that start only after four minutes of load|
|`idle-edge`|cores that fail idle at shallower offsets than under load (issue #106)|
|`misleading-mce`|shared-rail crashes that leave an MCE naming one core (issue #114)|

Iterate on `dev`. Run `holdout` only to confirm a result, so the holdout seeds stay unseen by the change being tuned.

Every run is a `tools/sim` subprocess with its own state directory, in parallel up to `--jobs`. A run that exceeds `--timeout` of wall time is killed and recorded as `timeout`. `--keep DIR` keeps the state directories under a new `DIR/bench-*/<scenario>/<split>-<seed>` (the path is printed), so the read-only commands can inspect a run with `--state-dir`.

## What a run records

`--out FILE` writes one JSON object per run:

- `status`: `concluded` (the session stopped after its clean qualifying rotation), `deadend`, `error` or `timeout`;
- `sim_hours`: simulated time from `session.start` to the last event, including the 90 s each crash reboot costs. This is the time to conclusion;
- `first_clean_rotation_h`: simulated time to the first clean qualifying rotation;
- `crashes`, `trials`, `trial_hours`, `hunts` and `joint_marks`;
- `final_profile` and `depth`, its sum;
- `hazard_per_h` and `hazard_max_per_h`: failures per hour at the final profile with every core loaded, per regime, from the simulator's own failure model. A profile that passed by luck shows up here, and no journal can show it.

The commit, a dirty flag and the ruleset are recorded with every run.

## Comparing two versions

Run the base version with `--out base.jsonl` and the candidate with `--baseline base.jsonl`. Runs pair by scenario and seed; a warning counts candidate runs without a base run, and base runs in the candidate's splits without a candidate run. For each pair the ratio is candidate `sim_hours` over base `sim_hours`. Each scenario reports the geometric mean of its ratios with a 95% bootstrap interval. The overall ratio weights scenarios equally, and its interval resamples pairs within each scenario. The last line is a verdict:

- `REJECT`: any violation, or the interval's lower bound above 1.0;
- `ACCEPT`: no violation and the interval's upper bound below 1.0;
- `NEUTRAL`: anything else.

Violations:

- **V1:** a run the base concluded no longer concludes.
- **V2:** a run's `hazard_max_per_h` rises by more than 0.01.
- **V3:** depth gets shallower: by more than 1 count averaged over a scenario, or by more than 5 in one run.
- **V4:** `shared-rail` gets slower overall.

Seeds are deterministic: the same commit always produces the same runs, so rerunning cannot change a result. A change to how the tuner decides moves later sessions onto different random paths, so compare whole scenarios, not single seeds.

Sessions stuck in joint hunts cost wall time as well as simulated time. Under ruleset 4, three of the eight `shared-rail` dev runs exceed the default 180 s timeout and count as not concluding. Raise `--timeout` when the base is that slow.

## Checking a machine against real evidence

A machine file may declare `facts = "../facts/target.jsonl.gz"`, resolved relative to the machine TOML. `shared-rail.toml` declares the committed target-machine extract. Files without `facts` are not checked.

Regenerate the extract from a temporary copy of the state directory, never the live state:

```sh
just facts COPY-OF-STATE-DIR
```

The generator reads every archived and live session through `internal/facts`, including sessions before `reset --all` and older builds. Its gzip JSON Lines output is byte-stable for fixed inputs. Generation rejects sources with no facts and writes to a temporary file beside the destination; the committed extract is replaced only after extraction and file close succeed, so an unreadable or empty source leaves it unchanged. Records retain session, build, ruleset, BIOS context, sequence, trial ID, kind, class, condition, phase, full profile, outcome, signal and measured duration. They omit journal timestamps, paths, hostnames and boot IDs. `tools/trialfacts` is the shared reader for evaluation tools.

The model check groups decisive starts by BIOS context, trial class (regime, workload, sorted loaded cores and intended duration), and the shallowest loaded-core offset in the applied profile. Any loaded core at 0 puts the start at depth 0. If the machine declares a BIOS context, only matching facts are checked; otherwise contexts are checked separately.

This check uses decisive pass/failure starts only; interrupted and inconclusive trials are omitted. A facts-declaring machine with positive `thermal_trip` or `power_loss` weights in `[model.reset]` is rejected: those resets can turn simulated crashes into inconclusive outcomes, while the simulator's failure probability includes the full failure hazard. Comparing that probability to decisive-only starts would use incompatible denominators.

For each group with at least 10 starts, the simulator computes each start's exact failure probability over its intended duration, including onset boosts, unloaded-core hazards and delayed joints. The check averages these probabilities and compares observed failures against quantiles 0.005 and 0.995 of Binomial(n, mean_p). A count outside that inclusive interval flags the machine. Idle failures are retained as their own fact class but have no start duration or exposure denominator, so the report counts them separately rather than inventing a per-start prediction.

`just bench` prints `ok` or `flagged` for each checked file, and every offending class, depth, n, k, interval and mean_p. JSON run records carry the same information in `model_check`, including all eligible groups. A flag says the file does not explain that part of the real evidence; it does not change the comparison verdict or exit status. Investigate the group before using improvements on that file as evidence about the target machine. This is a model diagnostic, not a claim that adaptive journal starts are independent or that all groups jointly have a 99% coverage guarantee.
