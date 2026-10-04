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
|`idle-limit`|cores that fail idle at shallower offsets than under load (issue #106)|
|`misleading-mce`|joint crashes that leave an MCE naming one core (issue #114)|
|`target-r4-limit`|target-fit-derived one-count together R4 limit gap on core 15; a single medium-duty checking trial can miss it (issue #107)|
|`target-nonmember-mce`|target-fit-derived CCD0 joint crashes deliberately name nonmember core 15; parked hunts can stop at that core's parked offset of zero (related to issue #114)|
|`target-delayed-joint`|target-fit-derived CCD1 joint delayed by two minutes; short checks miss it and finite long-trial coverage can still conclude unsafe (issue #105)|
|`target-flat-risk`|target-fit-derived rare offset-independent core-15 hazard that survives nonzero backoffs and finite checking evidence (issue #105)|
|`target-flat-cost`|target-fit-derived stronger core-15 flat hazard; repeated hunts and one-count backoffs exceed three times the unmodified median (issues #105, #106)|

Iterate on `dev`. Run `holdout` only to confirm a result, so the holdout seeds stay unseen by the change being tuned.

`--suite FILE` runs another scenario file instead, with machine paths relative to it. The [adversarial search](../tools/bench/adversary.md) scores candidate machines this way and turns confirmed findings into new scenarios.

The target scenario has 18 dev and 18 holdout seeds: two seeds per ensemble member in each split. Its oracle draws uniformly from decisive facts matching the entire applied profile and trial class (regime, workload, sorted loaded cores and intended duration). Draws are deterministic per seed and trial ID/index. Condition, phase, source ruleset and evidence epoch do not restrict a class match; BIOS context does. The oracle never uses facts from another BIOS context or records without one. Non-matching trials and trial-less failures use the fitted machine. The tuner sees only the resulting journal evidence, not the oracle or its extract.

Every run is a `tools/sim` subprocess with its own state directory, in parallel up to `--jobs`. A run that exceeds `--timeout` of wall time is killed and recorded as `timeout`. `--keep DIR` keeps the state directories under a new `DIR/bench-*/<scenario>/<split>-<seed>` (the path is printed), so the read-only commands can inspect a run with `--state-dir`.

Each simulator subprocess retains parsed events across its simulated reboots and writes `state.json` only at stop, avoiding journal re-parsing and per-event state-file rewrites. It still appends every event to `events.jsonl`; `--keep` leaves the journal and final state available for inspection. This changes wall-clock overhead, not simulated durations or tuner decisions. Recovery and interruption tests use the file-backed path, and fixed-seed equivalence tests compare both output files byte for byte on the default machine and `target-fit-0.toml`.

Trial samples stay in memory; retained sessions have no `trials/<trial-id>/samples.jsonl`. Their journal evidence, final state and read-only dashboards are unchanged. For per-second sample files, run `tools/sim` with `--samples` ([simulating](simulating.md)).

## Auditing journals

```sh
just audit                                        # every suite scenario, 200 seeds each; keeps a temporary evidence directory
just audit --seeds 50 --jobs 8 --keep runs          # change the sweep size, parallelism and retention directory
just audit COPY-OF-STATE-DIR OTHER-STATE-DIR         # read-only audit of current and archived journals
just audit --records RECORDS.jsonl --root BENCH-DIR # re-audit a retained bench sweep, including wall times
```

`tools/audit` composes `tools/bench --keep` with a generated suite: seeds 1 through `--seeds` for each scenario, preserving ensemble selection and replay settings. `--suite FILE` chooses another suite; `--timeout D` sets each simulator's wall timeout (default 180s). The printed evidence directory retains the generated suite, bench log, run records and state directories.

The audit checks recorded failure-point and combination avoidance independently of the tuner, SMU intent/write/readback ordering, the [-50, 0] bound on every offset togi chooses (readbacks and baselines report the hardware and are not bounded), causal references, simulated-run conclusions, and exact `state.json` replay when the snapshot's `last_seq` matches the replayed journal. A snapshot at another sequence is stale and is skipped: a crash between the journal fsync and the state write can leave it behind by design. A session from another ruleset has no current projection and is skipped too. Likewise, a `state.rebuilt` event is a violation only when its differing `fields` omit `last_seq`, meaning a current snapshot disagreed with replay. Real journals may still be live, including between a write and its readback. A write followed by a reboot or the end of an archive is an interrupted action, and a same-boot resume's readbacks of every core complete it; archived transitions and reset boundaries are valid session endings. Bench records additionally flag timeouts and runs exceeding ten times their scenario's median wall time. Every violation is a JSON object on stdout with its journal, session, sequence and reason; summaries go to stderr, and any violation exits nonzero. Audit copies of real state directories so the journal and state projection are a consistent snapshot.

## Proving unchanged decisions

For a shape-only change, run `just same [base]`; the base defaults to `origin/main` and is exported locally without fetching or registering a worktree. It builds the base and current simulators and runs each tree's own suite and machine files across every dev and holdout seed. Sessions pair by scenario, split and seed. Every current and archived journal pairs by its relative path and is compared byte for byte, after removing only `version`, `rev` and the exact build description in `msg` from `session.start` and `config.loaded`. Everything else, including `ruleset`, `schema`, `fixes` and `evidence_epoch`, must match.

Each differing session prints its first differing event's journal path, line number and both normalized lines. Missing sessions or journal files and differing simulator exit codes also count as differences. The command exits 0 when all sessions match and nonzero on a difference, execution error (including a timeout), or invalid arguments. A difference is fixed or split into its own issue, never explained away.

This is not a CI check: it needs two builds. `just bench` cannot prove equality because it compares metrics against thresholds; one changed decision with unchanged totals can pass.

To control concurrency, timeouts or retention directly, use `go run ./tools/bench --same DIR [--suite FILE] [--jobs N] [--timeout 180s] [--keep DIR]`. A relative suite path resolves inside each tree. This mode skips metrics, model checks and the bench summary; explicitly setting `--split`, `--baseline` or `--out` is a usage error. Each job runs one session from both trees and compares it. `--keep` retains separate `base/` and `head/` run sets under a new directory. Without it, runs go in a new directory under `togi` in the user cache directory (`$XDG_CACHE_HOME` or `~/.cache` on Linux, `~/Library/Caches` on macOS), each session's runs are removed as soon as they match, and only differing or failed sessions are retained, with their directory printed on stderr.

## What a run records

`--out FILE` writes one JSON object per run:

- `status`: `concluded` (the session stopped after its clean cycle), `deadend`, `error` or `timeout`;
- `sim_hours`: simulated time from `session.start` to the last event, including the 90 s each crash reboot costs. This is the time to conclusion;
- `first_passed_cycle_h`: simulated time to the first passed cycle;
- `crashes`, `trials`, `trial_hours`, `hunts` and `combinations`;
- `passed_cycles`: completed `checking.cycle` ends marked passed, whether full or not;
- `partial_seconds`: measured `trial.end.duration_s` summed for record-only trials belonging to those passed cycles, including passes, failures and inconclusive retries. Failed or unfinished cycles, unfinished trials, and trial-less crashes contribute no partial seconds.
- `real_answers` and `real_answer_share`: the number and fraction of completed trials answered by matching real facts; inconclusive trials and failures before workload startup count in `trials` but not as real answers;
- `scenario_real_answer_share`: real answers divided by completed trials across the scenario's selected runs, not an average of per-run fractions;
- `machine`: the fitted ensemble member selected for the seed.
- `final_profile` and `depth`, its sum;
- `hazard_per_h` and `hazard_max_per_h`: failures per hour at the final profile with every core loaded, per regime, from the simulator's own failure model. A profile that passed by luck shows up here, and no journal can show it.
- `worst_r7_hazard_per_h`, only on shared-voltage machines: the worst final-profile failures per hour across every R7 workload's full load on each CCD, each CCD's request-ordered partial chain with at least two loaded cores, and the all-core load. Each partial removes all loaded cores within 1 mV of the current highest request together, then recomputes requests with the resulting clocks and load before removing the next group. A removal leaving fewer than two cores ends that CCD's chain. Requests come directly from the shared-voltage model, and rates use the same simulator hazard as trials, without synthetic samples or random draws.

The commit, a dirty flag and the ruleset are recorded with every run.

The summary prints each scenario's `real_answer_share` and a pooled total. Comparison scenario rows and the verdict line print the candidate and baseline shares over paired runs. These fractions describe how much of the observed path has direct real evidence, not a confidence score. Unfinished trials in a timed-out subprocess have no recorded outcome and are not counted. Hazard metrics and model checks still describe the fitted fallback, not an empirical oracle hazard.

The summary's `partial_s/passed_cycle` divides pooled `partial_seconds` by pooled `passed_cycles`, not by runs or full cycles and not by an average of per-run ratios. It reports zero when no passed cycle completed. This is the elapsed load cost of record-only R7 partials per completed passed cycle: failed partials still cost their measured elapsed time, not their intended duration. It excludes reboot and other non-load overhead. Record-only outcomes remain normal trial ends and retained facts (including carried facts), but never tuner decision evidence; their marker does not change the trial class.

For shared-voltage runs the summary also prints `worst_r7_hazard/h`, the maximum `worst_r7_hazard_per_h` over the scenario's measured runs, and `worst_r7_runs`, their count. Legacy machines omit the JSON field and these diagnostic rows; their existing hazard metrics and reports remain unchanged.

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

Comparison rows and the verdict's diagnostic row report `partial_s_per_passed_cycle` and `baseline_partial_s_per_passed_cycle`. Each side pools partial seconds and passed cycles separately over the paired runs only. Older baseline records missing these fields contribute zero; a wholly older baseline therefore reports zero partial seconds per passed cycle. The candidate minus baseline value quantifies the added partial load time. This metric is informational and does not change the normal verdict or violations: issue #105's record-only layer is exempt from the benchmark gate.

When both sides of a pair recorded `worst_r7_hazard_per_h`, comparison also reports `max_worst_r7_hazard_delta` (the largest candidate-minus-baseline difference) and `worst_r7_pairs` (the number of such pairs). Missing values are unavailable, not zero: older baselines cannot supply this comparison. This metric is diagnostic only; V2 continues to use `hazard_max_per_h`, and no verdict threshold changes.

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

The generator reads every archived and live session through `internal/facts`, including sessions before `reset --all` and older builds. Its gzip JSON Lines output is byte-stable for fixed inputs. Generation rejects sources with no facts and writes to a temporary file beside the destination; the committed extract is replaced only after extraction and file close succeed, so an unreadable or empty source leaves it unchanged. Records retain session, build, ruleset, BIOS context, sequence, trial ID, kind, class, condition, phase, record-only status, full profile, outcome, signal, measured duration and, when present, `voltage_requests_v`, `top_requesters`, `ccd_mhz` and `stalled_core`. These are machine measurements, not personal data. For older trial ends without request fields, the generator summarizes surviving trial samples using the source session's recorded topology and loaded cores, with the same warmup and minimum-sample rules as live telemetry. They omit journal timestamps, paths, hostnames and boot IDs. `tools/trialfacts` is the shared reader for evaluation tools.
The extract also retains an optional attributed `core` on failures, including core 0, so a named computation error constrains the core it actually names rather than the first loaded core. Older records omit it.

The model check groups decisive trials by BIOS context, trial class (regime, workload, sorted loaded cores and intended duration), and the shallowest loaded-core offset in the applied profile. Any loaded core at 0 puts the trial at depth 0. If the machine declares a BIOS context, only matching facts are checked; otherwise contexts are checked separately.

This check uses decisive pass/failure trials only; interrupted and inconclusive trials are omitted. A facts-declaring machine with positive `thermal_trip` or `power_loss` weights in `[model.reset]` is rejected: those resets can turn simulated crashes into inconclusive outcomes, while the simulator's failure probability includes the full failure hazard. Comparing that probability to decisive-only trials would use incompatible denominators.

For each group with at least 10 trials, the simulator computes each trial's exact failure probability over its intended duration, including onset boosts, unloaded-core hazards and delayed joints. The check averages these probabilities and compares observed failures against quantiles 0.005 and 0.995 of Binomial(n, mean_p). A count outside that inclusive interval flags the machine. Idle failures are retained as their own fact class but have no trial duration or exposure denominator, so the report counts them separately rather than inventing a per-trial prediction.

`just bench` prints `ok` or `flagged` for each checked file, and every offending class, depth, n, k, interval and mean_p. JSON run records carry the same information in `model_check`, including all eligible groups. A flag says the file does not explain that part of the real evidence; it does not change the comparison verdict or exit status. Investigate the group before using improvements on that file as evidence about the target machine. This is a model diagnostic, not a claim that adaptive journal trials are independent or that all groups jointly have a 99% coverage guarantee.

## Fitting the target machine

```sh
just fit
# Optional: --facts EXTRACT.jsonl.gz --out DIRECTORY --seed 263 --bootstrap 8 --jobs N
```

`tools/fit` reads the same privacy-safe extract as the model check. It writes `target-fit-0.toml` from all decisive trials and `target-fit-1.toml` through `target-fit-8.toml` from whole-trial bootstrap samples drawn with replacement. The default seed is 263; refit `n` uses seed `263+n`. Every file declares its extract relative to the output directory and its BIOS context. Mixed-context extracts are refused: split them before fitting. The `target` scenario spreads separate dev and holdout seeds across all nine files, with the replay oracle above each.

The independent fits run in parallel up to `--jobs`, which defaults to the CPU count. Unconstrained fits finish before any flagged bootstrap members restart from the all-facts fit; those constrained refits also run in parallel. `--jobs 1` runs the same fits serially. Machine bytes and report order do not depend on the worker count.

Each bootstrap sample is fitted without constraints first. If its model check against the original extract flags a group, the fitter restarts from the passing all-facts fit and maximizes that **same resampled likelihood**, accepting only parameter moves that remain inside every original group's unchanged 99% interval. It does not redraw the sample, tune the seed, change the interval or repair the observed counts. This is a **checked, constrained bootstrap ensemble**: its spread is truncated to what the real facts admit, not an unconstrained bootstrap confidence interval. The fit output identifies each constrained refit and its flagged groups, including the unconstrained interval and mean probability. Each generated header records the bootstrap index and constrained groups' class, depth and observed counts; the usual final model-check report still checks the serialized machine files.

Before the telemetry refresh, seed-263 refits **2 and 8** required the constraint on `R7 mprime-avx2-36k-248k-allcore`, cores 0–7, 120 s, depth -24 (`n=45`, `k=5`). Their unconstrained predictions were respectively `mean_p=0.0178977`, interval `[0,4]`, and `mean_p=0.0125439`, interval `[0,3]`. That ensemble reported `ok` on its then-current 44 eligible groups. These historical numbers do not validate regenerated machines against the refreshed evidence; use the current `just fit` model-check report.

Against the refreshed evidence, the legacy all-facts fit itself fails the model check, so `just fit` writes `target-fit-0.toml`, then stops at the first bootstrap refit that needs a constraint (refit 1 with the default seed). The committed `target-fit-*` files predate the refresh and stay until a shared-voltage fit meets the forward bar below; after a stopped run, restore `target-fit-0.toml` from git.

The objective is the Bernoulli negative log likelihood of observed passes and failures, using `sim.Machine.FailureProbability` over each trial's **intended**, not measured, duration. Equal profile/class observations are aggregated without losing their counts. This includes unloaded-core failures and uses exactly the prediction the model check uses. A bounded coordinate search fits integer per-core alone/together regime limits, the shared past-limit rate and growth, the shared near-limit rate, per-core flat rates and unloaded-core idle limits. R7 failures support layered CCD shared-rail joints: each CCD starts with one all-member joint, and additional failure-profile candidates are accepted when they improve negative log likelihood by at least 0.5, up to eight layers per CCD. Joint member thresholds are fitted independently, not forced to a common depth. Workload limit overrides require at least 10 loaded trials, a failure, and the same minimum likelihood improvement. Search stops after twelve sweeps or negligible likelihood improvement; it is a local maximum-likelihood fit conditional on the selected structure, not a guarantee of a global optimum.

Each sweep also searches coupled integer limit/rate shifts. Moving active per-core, workload and idle limits by `delta` and scaling the past-limit rate by `growth^(-delta)` preserves already-past-limit hazards until a profile crosses a limit. This lets the search remove false hazards at observed clean boundaries instead of getting stuck when separate limit and rate moves each worsen the likelihood. Unsupported -50 limits stay fixed; joints are not shifted, and the same rate/limit bounds and original-evidence constraints still apply.

The positive-rate search covers past-limit rates from `1e-8` to `0.5` failures/s, near-limit rates from `1e-8` to `0.001`/s, flat rates from `1e-10` to `0.001`/s, and joint rates from `1e-7` to `0.5`/s. Growth ranges from `1.05` to `8`. Zero is also considered for past-limit, near-limit and flat rates; the joint search's zero candidate is clamped to `1e-12`/s because the simulator interprets a literal zero joint rate as the default past-limit rate. Limits range from -50 to 1, matching the simulator's stock-failure representation.

The fitter retains the original limit/joint fit, then adds a smooth loaded-CCD `[ccd]` hazard only where no fitted joint on that CCD applies. The residual stage freezes the existing joints and introduces no new joint candidates. It fits a shared log rate, nonnegative shared depth slope and two CCD effects with unit-normal shrinkage toward zero. Rate bounds are `1e-7` to `0.1`/s (zero is represented by `1e-12`/s), slope is zero or `1e-4` to `0.5` per mean depth count, and effects are bounded to [−5, 5]. Reported likelihood includes shrinkage. Existing machine files without `[ccd]` retain the original model.

The extract cannot identify every simulator parameter. Unsupported or all-passing limit boundaries stay at -50; absence of failures does not prove that boundary. The fit shares the hazard shape across cores and regimes because sparse failures cannot resolve a separate shape for each. Unsupported workload overrides stay absent; flat rates stay zero without supporting failures. Onset boost and joint delays stay at zero: decisive binary trials do not identify failure-time shapes. Signal weights, crash-MCE probability, bank attribution and reset kinds retain simulator defaults; these are not parameters of the binary likelihood. Idle facts without trials are counted by the check but provide no idle exposure denominator. Idle limits can be fitted only through decisive trials with nonzero offsets on unloaded cores; otherwise they stay disabled. Offset-zero unloaded trials also constrain idle-limit candidates, since a limit of 1 adds a hazard at stock offsets. Failure-only joint activation sets identify a lower bound, not an upper bound on the rate; a saturated fitted rate is a deterministic representative on that likelihood plateau, not a precise physical measurement. Generated headers summarize the fitted parameter families, unsupported-limit and unobserved-idle limits, and the fixed onset, delay, signal, MCE and reset settings.

Generation prints likelihoods, CCD joint parameters, model checks against the **original** extract (also for bootstrap fits), and elapsed wall time. Files contain no timestamps and use stable ordering and full-precision parameters, so fixed inputs and seed reproduce them byte for byte on one architecture. Between amd64 and arm64 they can differ in the last digits: Go implements its math functions separately for each architecture, and the arm64 compiler fuses multiply-adds. A last-digit difference after refitting on the other architecture is not a model change. A model-check flag is not silently repaired or excluded: inspect the named class, depth and interval before using that ensemble member as target evidence. A flag may reflect a poor local optimum, the current model's shared-shape/joint assumptions, or a sparse failure absent from a bootstrap sample; it is not by itself proof of an impossible fit. The shared `tools/modelcheck` implementation is used by both `just fit` and `just bench`, with the same 99% intervals.

### In-sample shared-voltage anchor

```sh
just fit-shared-voltage
# Optional: --facts EXTRACT.jsonl.gz --out DIRECTORY
# Equivalent: go run ./tools/fit --shared-voltage-in-sample
```

This opt-in mode fits **all decisive starts**, including record-only facts, to the 16-core shared-voltage model and writes only `target-shared-voltage.toml`. It requires one nonempty BIOS context and 16-core profiles. It leaves the default `just fit`, `just forward`, bootstrap dispatch and `target-fit-*` files unchanged. The anchor mode rejects `--bootstrap`, `--seed`, `--forward-only` and `--seal`, even when their supplied values equal their defaults.

Before creating the output directory or changing an anchor, it runs the **unchanged model check on the full original extract**, not a bootstrap or selected subset. It reports `ok` only when every eligible group passes (52 groups on the committed telemetry refresh). A non-ok result prints each exact flagged class, loaded cores, intended duration, depth, observed counts, interval and full-precision mean probability, then exits with failure without writing. A passing in-sample check does not validate later-session predictions.

If the unconstrained all-facts fit flags an original-evidence group, constrained scalar searches first profile the existing `shared_voltage.rate` within its documented `1e-5`–0.03 failures/s bounds, then profile the existing `shared_voltage.background_rate` within 0–0.001 failures/s if needed. Each search holds thresholds, margin, the other rate and legacy parameters fixed. It finds the scalar's interval admitted by every unchanged original-evidence group, then minimizes the same all-facts objective inside that feasible interval. The model already integrates its per-time hazard over each intended duration; these searches add no parameters, duration adjustment, threshold-shift repair or wider voltage-margin tail. Counts, group definitions, intervals and scoring remain unchanged. If neither scalar search finds a feasible value, the command reports the remaining flagged groups and returns failure without writing an anchor.

The fit retains the legacy R1–R6 and single-core model, but replaces multi-core R7 joints and CCD residual hazard with a shared rail. Loaded requests are a per-core/workload base plus a count coefficient and a clock coefficient. A loaded core's required voltage is its threshold plus its own required-clock coefficient times `(CCD MHz - reference MHz) / 100`; the smooth shared hazard follows the margin between required voltage and the highest loaded request across both CCDs. Fitting uses request and clock measurements where available and named-core failure attribution. Its optimization includes threshold shrinkage of 10 mV within CCD/workload and a lognormal margin prior centered at 6 mV with log-2 width; the **reported raw Bernoulli log loss excludes priors and attribution penalties**.

An additional `shared_voltage.background_rate` represents a **CO-independent platform R7 background**: memory, heat or the rest of the platform can fail even when loaded cores have ample voltage margin, including at CO 0. This is one shared rate per multi-core R7 trial, independent of profile, load size and workload; it produces unattributed crashes and contributes to the same hazard and failure-probability predictions used by fitting and model checking. It is separate from the voltage-margin hazard, not a wider margin tail. Its default is zero; only finite nonnegative values are accepted. Legacy loaded-core `Flat` is suppressed in shared R7 and legacy CO-0 subtraction removes its stock hazard, so unchanged `Flat` cannot supply this mechanism.

Shared-voltage parameter bounds:

| Parameter | Range |
|---|---|
| Request count coefficient | 0.0025–0.005 V/count |
| Request and required-voltage clock coefficients | 0.005–0.06 V/100 MHz |
| Shared per-core rate | `1e-5`–0.03 failures/s |
| CO-independent platform R7 background rate | 0–0.001 failures/s |
| Smooth margin | 0.001–0.03 V |
| Absolute required-voltage thresholds | 0.8–1.5 V |
| Per-core/workload request bases | 0.9–1.5 V |
| Clock gain per unloaded core | 0–60 MHz/core |
| Package and CCD-balance clock responses | 0–30 MHz/W |
| Full-load CCD clock intercepts | 4000–6000 MHz |

The request reference is fixed at 5240 MHz. Power and thermal limits are fixed at 192 W; workload power is 13 W/core for mprime AVX2 and 15 W/core for AVX-512 and y-cruncher, plus 0.12 W per offset count relative to −35. Full-load clocks use measured medians; unsupported ones retain fixed priors. Unsupported y-cruncher R7 uses, per core, the more demanding (maximum) of the mprime AVX2/AVX-512 thresholds, not a hardware-validated threshold.

The generated header explicitly says **IN-SAMPLE, NOT forward-validated**, records the failed 2026-10-04 [#306](https://github.com/shgew/togi/issues/306#issuecomment-5979625589) bar, and declares relative `facts` and `[bios_context]`. Workloads and cores have stable order; floating-point parameters have full precision. Reports print raw total and per-start in-sample Bernoulli log loss for all starts and R7, and predicted/observed R7 counts **for record-only purposes**, not as held-out scores. These diagnostics also print for a flagged fit before failure returns without writing an anchor. Fixed inputs reproduce files and reports on one architecture, excluding the elapsed line.

**Q17 forward bar remains authoritative.** A shared-voltage fit used for forward claims or regeneration of `target-fit-*` must beat the constant predictor's log loss on a held-out session's R7 starts and predict total R7 failures within a factor of 2 ([#105](https://github.com/shgew/togi/issues/105)). The first candidate failed on 2026-10-04: 36.0 predicted against 17 observed (above the allowed 34), despite log loss 0.270 below constant 0.399. That check was not blind; it was not retuned afterwards, and a future forward claim needs a new blind session. This all-facts anchor neither reruns nor supersedes that check and does not authorize `target-fit-*` regeneration.

To replay exact real facts over the anchor, set `replay = true` on a **bench suite scenario**, not in the machine TOML (that key is unsupported there). For example, place this one-scenario suite at `tools/bench/shared-voltage-suite.toml`:

```toml
[[scenario]]
name = "target-shared-voltage-in-sample"
machine = "machines/target-shared-voltage.toml"
replay = true
dev = [1]
holdout = [101]
```

```sh
just bench --suite tools/bench/shared-voltage-suite.toml --split dev
```

Machine paths resolve relative to the suite; facts resolve relative to the machine. The declared facts and BIOS context let `trialfacts.LoadReplay` construct `sim.NewReplay` for matching decisive trials, with fitted fallback on unmatched classes/profiles. A replay run remains in-sample evidence, not forward validation.

### Forward-chained check

After the ensemble's model check, `just fit` orders sessions with decisive trials by their UTC session IDs, comparing equal-second numeric suffixes numerically (`-2` before `-10`). For each session after the first, it fits all earlier sessions' decisive trials once, without bootstrap resampling or model-check constraints, then predicts only the held-out session. These fits stay in memory and do not change the generated machine files.

The prefix fits run in parallel with the same `--jobs` limit. Scoring and pooling remain in session order, so no held-out outcomes enter their own training prefix and the report is unchanged apart from elapsed times.

Each session row names the session ID, ruleset and `training_sessions` count. `trials` and `failures` count held-out decisive trials and observed failures; `predicted` sums the fit's failure probabilities over their intended durations. `log_loss/trial` is the mean Bernoulli log loss, with probabilities clamped to `[1e-4, 1-1e-4]`. It compares the `fit` to a `constant` predictor whose probability, shown in parentheses, is the earlier training trials' failure rate. Lower loss is better: fit loss above the constant means the fit predicts that later session worse than a single average failure rate.

`failures_p<0.01` counts observed failures assigned less than 1% probability: outcomes the model treated as nearly impossible. `exact_matches` is the number and share of held-out trials whose trial class and full profile occur in the earlier training trials. That share bounds what replay could answer from earlier evidence; it does not establish that replay's answers would be correct. `flagged/eligible` counts held-out groups flagged by the unchanged model check, with at least 10 trials and the same 99% binomial intervals. The indented regime rows also show fit and constant log loss over the same regime's trials, using exactly the existing probability clamp and each session's prefix-wide constant.

`Pooled` sums all held-out sessions' counts and predictions and divides summed log losses by their total trials. Its constant uses each session's own earlier prefix, not one failure rate fitted to the pooled outcomes; its group counts sum the separate held-out checks. `Forward-chained elapsed` reports the added wall time. This is evidence about extrapolation to later sessions, not a gate or a guarantee about unseen profiles or other machines.

```sh
just forward            # the check alone: no ensemble, no machine files
just forward --seal 1   # also leave the newest session unfitted and unscored
```

`just forward` runs `tools/fit --forward-only`: the same rows and `Pooled` line, without fitting the ensemble or writing machine files. `--seal N`, accepted only with `--forward-only`, drops the newest N sessions before the check: they are neither fitted nor scored, the remaining rows are unchanged, and `Pooled` covers only the remaining held-out sessions. At least one held-out session must remain. Model research ([program](../tools/fit/program.md)) iterates with the newest session sealed and scores it only to confirm a kept change.

Before the joint-preserving residual hazard was added, the committed extract produced these reference rows with the default `just fit` arguments:

```text
Forward-chained check
20260926T151414Z ruleset=2 training_sessions=1: trials=479 failures=22 predicted=2.5 log_loss/trial fit=0.3478 constant(0.064)=0.1892 failures_p<0.01=20 exact_matches=44/479 (9.2%) flagged/eligible=0/0
  R1 trials=195 observed=1 predicted=0.4
  R2 trials=155 observed=17 predicted=0.3
  R3 trials=56 observed=0 predicted=0.1
  R4 trials=50 observed=0 predicted=0.1
  R5 trials=18 observed=2 predicted=0.0
  R6 trials=3 observed=0 predicted=0.1
  R7 trials=2 observed=2 predicted=1.6
20260927T221954Z ruleset=3 training_sessions=2: trials=403 failures=18 predicted=9.0 log_loss/trial fit=0.1841 constant(0.057)=0.1840 failures_p<0.01=10 exact_matches=89/403 (22.1%) flagged/eligible=0/0
  R1 trials=51 observed=0 predicted=0.0
  R2 trials=281 observed=4 predicted=0.4
  R3 trials=17 observed=0 predicted=0.0
  R4 trials=17 observed=0 predicted=0.0
  R5 trials=17 observed=1 predicted=0.0
  R7 trials=20 observed=13 predicted=8.6
20260929T180308Z ruleset=4 training_sessions=3: trials=894 failures=59 predicted=13.9 log_loss/trial fit=0.4597 constant(0.054)=0.2445 failures_p<0.01=44 exact_matches=10/894 (1.1%) flagged/eligible=6/8
  R1 trials=80 observed=0 predicted=0.0
  R2 trials=80 observed=0 predicted=0.0
  R7 trials=734 observed=59 predicted=13.9
20261002T004254Z ruleset=6 training_sessions=4: trials=317 failures=8 predicted=18.9 log_loss/trial fit=0.0666 constant(0.058)=0.1302 failures_p<0.01=0 exact_matches=246/317 (77.6%) flagged/eligible=1/4
  R1 trials=80 observed=0 predicted=0.0
  R2 trials=80 observed=0 predicted=0.0
  R7 trials=157 observed=8 predicted=18.9
Pooled: trials=2093 failures=107 predicted=44.4 log_loss/trial fit=0.3215 constant(per-prefix)=0.2029 failures_p<0.01=74 exact_matches=389/2093 (18.6%) flagged/eligible=7/12
```

In one development run, the four added prefix fits and held-out checks took about 43 s. This is an observed wall time, not a runtime benchmark.
