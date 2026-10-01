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
- `first_clean_rotation_h` and `bronze_h`: simulated time to those milestones;
- `crashes`, `trials`, `trial_hours`, `hunts` and `joint_marks`;
- `final_profile` and `depth`, its sum;
- `hazard_per_h` and `hazard_max_per_h`: failures per hour at the final profile with every core loaded, per regime, from the simulator's own failure model. A profile that passed by luck shows up here, and no journal can show it.

The commit, a dirty flag and the ruleset are recorded with every run.

## Comparing two versions

Run the base version with `--out base.jsonl` and the candidate with `--baseline base.jsonl`. Runs pair by scenario and seed. For each pair the ratio is candidate `sim_hours` over base `sim_hours`. Each scenario reports the geometric mean of its ratios with a 95% bootstrap interval. The overall ratio weights scenarios equally, and its interval resamples pairs within each scenario. The last line is a verdict:

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
