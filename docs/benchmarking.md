# Benchmarking

`tools/bench` runs a fixed suite of simulated sessions and compares two versions of togi. Use it to show that a tuner change makes sessions conclude sooner, or no later, without giving up depth or leaving a profile that fails. Like `tools/sim`, it is a development program and is not shipped.

For the autonomous experiment loop, frozen evaluation environment, proof standard and owner-controlled merge and deployment workflow, follow the [research program](../tools/bench/program.md). [ADR 0029](adr/0029-bench-verdicts-rest-on-fitted-machines.md) records why target claims require checked fits and real-fact replay.

```sh
just bench [--split dev|holdout|all] [--out FILE] [--baseline FILE] [--keep DIR] [--jobs N] [--max-boots 1000]
```

## The suite

`tools/bench/suite.toml` lists scenarios. Each scenario names a simulator `machine` file in `tools/bench/machines/` (none means the seeded default machine), or an ensemble in `machines`, and two sets of seeds, `dev` and `holdout`. Ensemble seeds select successive members round-robin by their position within each split, not by the seed value. `replay = true` enables the same-BIOS replay oracle over the selected fitted member:

|Scenario|Models|
|---|---|
|`default`|the seeded default machine|
|`target`|real-fact replay over the all-facts target fit and eight bootstrap refits, all flagged by the model check and the refits unconstrained ([ADR 0044](adr/0044-write-flagged-target-fits.md)), including CCD0 and CCD1 R7 joints|
|`flat-hazard`|rare failures at any nonzero offset on two cores (issue #105)|
|`late-onset`|R7 joint failures that start only after four minutes of load; the cores' own limits have no delay|
|`idle-limit`|cores that fail idle at shallower offsets than under load (issue #106)|
|`misleading-mce`|joint crashes that leave an MCE naming one core (issue #114)|
|`target-r4-limit`|target-fit-derived one-count together R4 limit gap on core 15; each medium-duty checking trial misses it 70.55% of the time, so the default cycle's three still can (issue #107)|
|`target-nonmember-mce`|target-fit-derived CCD0 joint crashes deliberately name nonmember core 15; parked hunts can stop at that core's parked offset of zero (related to issue #114)|
|`target-delayed-joint`|target-fit-derived CCD1 joint delayed by two minutes; short checks miss it and finite long-trial coverage can still conclude unsafe (issue #105)|
|`target-flat-risk`|target-fit-derived rare offset-independent core-15 hazard that survives nonzero backoffs and finite checking evidence (issue #105)|
|`target-flat-cost`|target-fit-derived stronger core-15 flat hazard; repeated hunts and one-count backoffs exceed three times the unmodified median (issues #105, #106)|
|`shared-voltage`|hand-set shared-rail R7 model with workload-dependent requests, clocks and per-core voltage demand; no facts or replay|
|`target-shared-voltage`|real-fact replay over the all-facts shared-voltage anchor; an in-sample fit, not forward-validated|
|`target-r7-vf-boost`|anchor-derived AVX2 core-5 required voltage rises faster than its request as partial CCD0 loads boost; in-sample, not forward-validated|
|`target-r7-request-gap`|anchor-derived AVX-512 required-voltage curves leave core 11 dependent on core 10's rail support: full CCD1 loads pass but partial loads without core 10 can fail; requests unchanged; in-sample, not forward-validated|

The failure sources are separate from the signals they produce:

| Scenarios | Failure sources |
|---|---|
| `default` | Seeded loaded-core limits. No unloaded-core hazard or joint. |
| `flat-hazard`, `idle-limit`, `late-onset`, `misleading-mce` | Loaded-core limits, plus respectively nonzero-offset flat hazards on loaded or unloaded cores, unloaded-core idle limits, delayed R7 joints, or R7 joints with misleading MCE attribution. |
| `target` | Exact real-fact answers where matched; otherwise fitted loaded-core limits, any fitted flat hazards, R7 joints and the residual `[ccd]` hazard where no joint applies. |
| `target-r4-limit`, `target-nonmember-mce`, `target-delayed-joint`, `target-flat-risk`, `target-flat-cost` | Exact real-fact answers where matched; otherwise the legacy limit/joint fit with the named adversary change. Flat hazards also apply when their cores are unloaded. |
| `shared-voltage` | Loaded-core voltage-margin hazards for multi-core R7; loaded-core limits elsewhere. No replay, unloaded-core hazard or platform background rate. |
| `target-shared-voltage`, `target-r7-vf-boost`, `target-r7-request-gap` | Exact real-fact answers where matched; otherwise loaded-core voltage-margin hazards and a CO-independent platform background rate for multi-core R7, and fitted loaded-core limits elsewhere. No unloaded-core hazard. |

On `target-shared-voltage`, changing only unloaded cores' offsets cannot change the fitted multi-core R7 hazard. All `flat` rates are zero and no core has an `idle` limit; the voltage model computes power, clocks and the rail from loaded cores only. The same is true of its two shared-voltage adversaries. Parking unloaded cores can still change which real facts match, since replay matches the **whole** profile. A failure during a parked locate on this anchor is therefore not proof of an unloaded-core voltage dependency: it can come from a loaded core, the background rate or replay. This is a limitation of these scenarios, not a statement about hardware ([simulator model](simulating.md#shared-voltage-r7-model)).

Iterate on `dev`. Run `holdout` only to confirm a result, so the holdout seeds stay unseen by the change being tuned.

### The gate

The [`[gate]` table in `tools/bench/suite.toml`](../tools/bench/suite.toml) is the open ruleset's bench gate, committed before the ruleset is scored. It names the gate `id`, the `baseline` it is judged against (the ruleset and the commits that recorded the runs; a pull request that re-records runs adds its commit), the seed `split`, the `gated` scenarios, the `pooled` subset, the `conclude` scenarios, the `quantiles`, the bootstrap (`confidence`, `resamples`, `bootstrap_seed`) and `max_time_ratio`. The table and its `note` are the definition and the rationale; this page does not restate them per ruleset, and [ADR 0040](adr/0040-located-hunts.md) records the Ruleset 10 gate it replaced. The gate comes from [issue #491](https://github.com/shgew/togi/issues/491) and [issue #570](https://github.com/shgew/togi/issues/570).

With `--baseline`, `just bench` judges the gate after the generic verdict below, prints each criterion with its estimate, interval, threshold and `PASS` or `FAIL`, then an overall `gate ID: PASS` or `FAIL`, and exits 0 only when the gate passes. Run it as `just bench --split all --baseline tools/bench/baseline.jsonl`, scored once on every seed of the gate's split; a different `--split`, a candidate or baseline run missing for a required seed, a baseline recorded at another ruleset or commit than the gate names, or a missing `worst_r7_hazard_per_h` fails the gate with a refusal that names the cause. A suite without a `[gate]` prints no gate and exits as before. The criteria, per scenario unless noted:

- **Conclusion:** every seed the baseline concluded is still concluded by the candidate, on each gated and each `conclude` scenario.
- **Median and 90th percentile** (linear interpolation) of the final-profile `worst_r7_hazard_per_h`: fails only when the `confidence` interval of the change lies entirely above 0. The change is the candidate's quantile minus the baseline's, not the quantile of per-pair differences. The interval resamples seed pairs `resamples` times, drawing from a fresh PCG seeded with `bootstrap_seed`, so a run always prints the same interval. Over 24 seeds the interval is wide, so the 90th-percentile criterion has low power: it catches a large tail rise, not a small one.
- **Time:** the geometric mean over pairs both concluded of candidate `sim_hours` over baseline's is at most `max_time_ratio`, with no exemption.
- **Pooled median:** the median over the `pooled` scenarios' seeds together is strictly lower than the baseline's; equal fails.

Maxima and everything else are reported by the summary and the comparison, not gated. Treat a failed gate as the owner's decision, not a threshold to renegotiate after scoring.

### Historical measurements

The **historical score of 2026-10-07**, recorded in [ADR 0040](adr/0040-located-hunts.md), used the anchor merged at `0ccbe089` (generated at `0f6b5d23`), before its signal-mix regeneration in [#461](https://github.com/shgew/togi/pull/461), merge `bf7230d`. Those gate numbers are simulated results on that earlier evaluation environment, not current-anchor timings.

On the regenerated anchor (`18a51a63`, merged at `bf7230d`), the [2026-10-07 #493 measurements](https://github.com/shgew/togi/issues/493#issuecomment-6040930168) report **simulated** median total time of 53.1 h for ruleset 9 and 249.8 h for ruleset 10, about 4.7×, on `target-shared-voltage` with 24 seeds. Ruleset 10 ran at `645f529`; ruleset 9 ran at `84c2a01` with the same simulator and regenerated machine. That anchor remains unchanged at `2f318490` (2026-10-10). The earlier 1.90×/2.03× claims do not describe it. See [How togi tunes](how-togi-tunes.md) for the measured phase breakdown and [the reproduction recipe](#reproducing-an-older-ruleset).

A scenario may also list `smoke` seeds, each one of its dev or holdout seeds: the sessions `just smoke` compares ([below](#proving-unchanged-decisions)). The list holds one session per scenario, the one with the most crashes and the longest history that takes about two seconds of CPU or less on the current simulator, so it changes in a reviewed pull request, never silently.

`--suite FILE` runs another scenario file instead, with machine paths relative to it. The [adversarial search](../tools/bench/adversary.md) scores candidate machines this way and turns confirmed findings into new scenarios.

The target scenario has 18 dev and 18 holdout seeds: two seeds per ensemble member in each split. Its oracle draws uniformly from decisive facts matching the entire applied profile and trial class (regime, workload, sorted loaded cores and intended duration). Draws are deterministic per seed and trial ID/index. Condition, phase, source ruleset and evidence epoch do not restrict a class match; BIOS context does. The oracle never uses facts from another BIOS context or records without one. Non-matching trials and trial-less failures use the fitted machine. The tuner sees only the resulting journal evidence, not the oracle or its extract.

Every run is a `tools/sim` subprocess with its own state directory, in parallel up to `--jobs`. `--max-boots` bounds each session by simulated work, not wall time: a session still running after that many simulated boots stops with `censored` status ([simulating](simulating.md)), so a slow or loaded host makes a run take longer but never changes its status. `--timeout` (default 30m) is only a safety net against a hung simulator: a run that exceeds it is killed and the command fails with a harness error naming the session, never recording a session outcome. Runs start longest first, by the wall times recorded by earlier runs in the user cache directory (`togi/bench/costs.json` under `$XDG_CACHE_HOME` or `~/.cache` on Linux, `~/Library/Caches` on macOS), and in suite order for sessions without a record. The first failed run cancels the rest. SIGINT and SIGTERM kill the simulators and remove the temporary directories. `--keep DIR` keeps the state directories under a new `DIR/bench-*/<scenario>/<split>-<seed>` (the path is printed), so the read-only commands can inspect a run with `--state-dir`. Without `--keep`, each run's state directory is removed as soon as its metrics are read, so a full-suite run holds at most `--jobs` of them at once.

Each simulator subprocess retains parsed events across its simulated reboots and writes `state.json` only at stop, avoiding journal re-parsing and per-event state-file rewrites. It still appends every event to `events.jsonl`; `--keep` leaves the journal and final state available for inspection. This changes wall-clock overhead, not simulated durations or tuner decisions. Recovery and interruption tests use the file-backed path, and fixed-seed equivalence tests compare both output files byte for byte on the default machine and `target-fit-0.toml`.

Trial samples stay in memory; retained sessions have no `trials/<trial-id>/samples.jsonl`. Their journal evidence, final state and read-only dashboards are unchanged. For per-second sample files, run `tools/sim` with `--samples` ([simulating](simulating.md)).

### Running one scenario

There is no `--scenario` flag. Write a scratch suite containing just the scenario, with an absolute machine path so the suite may live outside the checkout. From the checkout, in the dev shell:

```sh
scratch=$(mktemp -d)
repo=$(pwd -P)
cat > "$scratch/suite.toml" <<EOF
[[scenario]]
name = "target-shared-voltage"
machine = "$repo/tools/bench/machines/target-shared-voltage.toml"
replay = true
dev = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
holdout = [101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112]
EOF
go run ./tools/bench --suite "$scratch/suite.toml" --split all \
  --keep "$scratch/runs" --out "$scratch/results.jsonl" --timeout 15m
```

Keep `scratch` until inspection is finished. To select another scenario, copy its seed lists and `replay` setting from `tools/bench/suite.toml`. For an ensemble, use `machines = ["ABSOLUTE-MEMBER-0.toml", "ABSOLUTE-MEMBER-1.toml"]` instead of `machine`. Facts paths still resolve relative to each machine file.

### Reproducing an older ruleset

Pin the tuner, simulator, machine and facts separately. These are the commits used by the [#493 measurements](https://github.com/shgew/togi/issues/493#issuecomment-6040930168), not a claim that an arbitrary later `main` gives the same numbers:

| Ruleset | Tuner reference | Shared simulator reference |
|---|---|---|
| 9 | `84c2a01` | `645f529` |
| 10 | `c660ba6` (the ruleset-10 merge) | `645f529` (the measured main checkout) |

The ruleset-9 port needs `ab7b9b2` and only the `internal/sim` changes of `e559b81`, plus the regenerated machine. Restoring the named paths from the shared reference gives that same simulator and also keeps facts together with their machine. Run from the repository:

```sh
sim_ref=645f529
older=$(mktemp -d)
git worktree add --detach "$older/ruleset9" 84c2a01
git -C "$older/ruleset9" restore --source="$sim_ref" --staged --worktree -- \
  internal/sim tools/sim \
  tools/bench/machines/target-shared-voltage.toml tools/bench/facts
git -C "$older/ruleset9" diff --exit-code "$sim_ref" -- \
  internal/sim tools/sim \
  tools/bench/machines/target-shared-voltage.toml tools/bench/facts
```

The diff must be empty. In that detached worktree, enter the dev shell and run the one-scenario recipe above. For ruleset 10, use a detached worktree at `645f529` without the port. Both runs must use the same scenario name, seeds, machine and replay setting. Preserve the worktrees and scratch results until analysis ends; their diffs explain the deliberately dirty ruleset-9 records.

For a comparison on a later simulator, review **every** commit that changed the simulator paths since the older ruleset, not just those two signal-mix commits:

```sh
git log --reverse --no-merges --format='%h %s' 84c2a01..origin/main -- \
  internal/sim tools/sim
git log --oneline 645f529..origin/main -- internal/sim tools/sim
```

The two-commit port is not sufficient for main at `2f318490` (2026-10-10). Later changes couple the simulator to `internal/simrun`, session recovery and journal buffering. `0ea69fc1` also changes samples from maps to `machine.PerCore`, with corresponding changes in requests and session code. Copying only `internal/sim` and `tools/sim` then fails to build against the older infrastructure. A current-simulator port must carry those dependencies without importing newer tuning decisions, and still leave the simulator, machine and facts diff empty against its pinned reference. A generic current-main port procedure moves to [the redesign, #493](https://github.com/shgew/togi/issues/493): this needs an infrastructure compatibility boundary, not a guessed cherry-pick list. The pinned recipe above reproduces the documented measurements.

## Auditing journals

```sh
just audit                                        # every suite scenario, 200 seeds each; keeps a temporary evidence directory
just audit --seeds 50 --jobs 8 --keep runs          # change the sweep size, parallelism and retention directory
just audit COPY-OF-STATE-DIR OTHER-STATE-DIR         # read-only audit of current and archived journals
just audit --records RECORDS.jsonl --root BENCH-DIR # re-audit a retained bench sweep, including wall times
```

`tools/audit` composes `tools/bench --keep` with a generated suite: seeds 1 through `--seeds` for each scenario, preserving ensemble selection and replay settings. `--suite FILE` chooses another suite; `--timeout D` sets bench's hang safety limit per simulator (default 30m); sessions are bounded by simulated boots, not wall time. The printed evidence directory retains the generated suite, bench log, run records and state directories.

The audit checks recorded failure-point and combination avoidance independently of the tuner, SMU intent/write/readback ordering, the offset range ([Invariants](spec/tuner.md#invariants); readbacks and baselines report the hardware and are not bounded), causal references, simulated-run conclusions, and exact `state.json` replay when the snapshot's `last_seq` matches the replayed journal. A multi-core R7 failure naming a core at CO 0 records a failure point only when a dead end cites it ([R7 request order and attribution](spec/tuner.md#r7-request-order-and-attribution)). A snapshot at another sequence is stale and is skipped: a crash between the journal fsync and the state write can leave it behind by design. A session from another ruleset has no current projection and is skipped too. So is a session started by an older journal schema: replay always stamps the current schema, and the next run archives that session. Likewise, a `state.rebuilt` event is a violation only when its differing `fields` omit `last_seq`, meaning a current snapshot disagreed with replay. Real journals may still be live, including between a write and its readback. A write followed by a reboot or the end of an archive is an interrupted action, and a same-boot resume's readbacks of every core complete it; archived transitions and reset boundaries are valid session endings. Bench records additionally flag runs exceeding ten times their scenario's median wall time. Every violation is a JSON object on stdout with its journal, session, sequence and reason; summaries go to stderr, and any violation exits nonzero. Audit copies of real state directories so the journal and state projection are a consistent snapshot.

## Proving unchanged decisions

For a shape-only change, `just same [base]` proves that decisions are unchanged on simulated paths only: the simulator stands in for `internal/trial`, `internal/detect`, `internal/smu`, `internal/hardware` and the backends, so it proves nothing about a change to them ([what each check proves](#what-each-check-proves)). The base defaults to `origin/main` and is exported locally without fetching or registering a worktree. It builds the base and current simulators with `-trimpath -buildvcs=false` and runs each tree's own suite and machine files across every dev and holdout seed. Sessions pair by scenario, split and seed. Every current and archived journal pairs by its relative path and is compared byte for byte, after removing only `version`, `rev` and the exact build description in `msg` from `session.start` and `config.loaded`. Everything else, including `ruleset`, `schema`, `fixes` and `evidence_epoch`, must match.

Each side's run is reduced to a digest of its normalized journals and its exit status, which the command caches in the user cache directory (`togi/bench` under `$XDG_CACHE_HOME` or `~/.cache` on Linux, `~/Library/Caches` on macOS) under a key made of the simulator binary's SHA-256, `--max-boots` and the sessions with the machine and facts files they read. The binary carries the code, the Go version and the platform, and the flags make it byte for byte reproducible, so a base that does not change the simulator is a cache hit across commits, and a rerun after an edit that leaves the simulator unchanged (a comment, a doc, a test) runs nothing. When both trees build the same simulator and read the same inputs, the command says so and reports no difference without running any session. Every other session runs once per side that lacks a record, longest first, as independent jobs, and each run directory is removed as soon as its digest is taken and its pair compared. Only the cache keeps what a run proved; the cache keeps the eight most recently used keys.

A difference stops the comparison: the remaining runs are cancelled and the first difference prints. `--keep-going` runs every session and counts every difference. A differing session prints its first differing event's journal path, line number and both normalized lines; its run directories are retained and their directory printed on stderr, after rerunning any side that was cached or already deleted. Missing sessions or journal files and differing simulator exit codes also count as differences. The command exits 0 when all sessions match and nonzero on a difference, execution error (a simulator that exceeds the hang safety limit included) or invalid arguments. A difference is fixed or split into its own issue, never explained away. A session that differs only because of a stale cache entry shows `<journal digest>`; rerun with `--no-cache` to rule that out.

Every session's CPU time, user plus system, is part of its cached record, so the report compares head with base even when the base ran in an earlier invocation. It prints the total of both sides and every session whose head took more than twice its base's CPU time, the ten worst first. That is informational: it never changes the verdict or the exit status.

`just smoke [base]` is the same comparison, with the same cache, fail-fast and flags, over only the sessions the suite lists under `smoke`: a quick check while editing, to run `just same` after. A session the base suite lacks counts as a difference.

This is not a CI check: it needs two builds. `just bench` cannot prove equality because it compares metrics against thresholds; one changed decision with unchanged totals can pass.

To control concurrency, caching or retention directly, use `go run ./tools/bench --same DIR [--suite FILE] [--jobs N] [--max-boots N] [--timeout 30m] [--smoke] [--keep-going] [--no-cache] [--cache DIR] [--keep DIR]`. A relative suite path resolves inside each tree. This mode skips metrics, model checks and the bench summary; explicitly setting `--split`, `--baseline` or `--out` is a usage error. `--no-cache` ignores cached digests and runs every session on both trees, still recording them; `--keep` implies it and retains every run, in separate `base/` and `head/` sets under a new directory. Without `--keep`, runs go in a new directory under `runs` in the cache directory, which `--cache DIR` moves. SIGINT and SIGTERM kill the simulators and remove the temporary directories, and a failed run keeps only its own directory.

## What each check proves

Cite a check as evidence only for the packages it exercises. `just gate` runs the integration tag, the race set and `module`; `just check` runs every flake check the host builds, including the Linux-only `trial-scope-tests`, `vm` and `vm-restart-limit`.

| Check | Runs | Exercises | Does not prove |
|---|---|---|---|
| `just test`, `just focus` | `go test` on `./...` or the named packages, with no build tag; on Linux this includes `_linux_test.go` files | Every package's ordinary tests, tools included: the tuner, session, carry and journal directly, and trial, detect, smu, hardware and the backends against fakes, fake file trees and fake package trees | That a stress program accepts its generated configuration; real systemd scopes, kernel logs or SMU access |
| Integration tag | `go test -shuffle=on -tags integration ./...` in `just gate` and in the flake `package` check's test phase | Everything `just test` does, plus, on Linux only, trial's process tests (real child processes, process groups, signals and sweeps, and a hung `systemctl` stand-in for the scope deadline), detect's `journalctl` deadline and sim's sample persistence failures; on any host, the nushell completion script and `tools/cover` and `tools/release` against temporary git repositories | Real systemd scopes, stress programs, kernel logs or SMU access; on macOS, the Linux-only files are not built |
| Race set | `go test -race -shuffle=on -tags integration` on `./internal/trial`, `./internal/journal`, `./internal/watch`, `./internal/smu` and `./cmd/togi`, in `just gate` and the flake `race` check | Those five packages' tests under the race detector, and the code of other packages only as far as those tests reach it | Race freedom of any other package's tests or of paths those tests do not reach |
| `module` | Evaluates `nix/module.nix` for x86_64-linux on any host (`nix/module-test.nix`) | The module's options and generated configuration: GRUB mirror constraints, the grubenv mount dependency, the tuning specialisation's 30 s watchdog and kernel parameters, the restart-limit and leave-tuning-boot scripts, the lock file rule and package overrides | Any Go code, or that the configuration boots and behaves as evaluated |
| `trial-scope-tests` | Linux only: `go test -c -tags hardware ./internal/trial` from the shipped files and trial's hardware test files | That trial's hardware-tagged tests compile | Anything at run time: `vm` runs the binary |
| `vm` | Linux only: a NixOS VM with the module and its tuning specialisation (`nix/vm-test.nix`) | trial's `^TestHardwareScope` tests as root under real systemd and `^TestHardwareScope$` as a delegated user, with helper backends; the documented lock provisioning; the installed `togi`'s host-lock refusals, its dead end at preflight on the tuning boot and the saved entry it clears; the watch and console services; booting the GRUB tuning entry; the armed watchdog's reset when PID 1 freezes | Real stress programs, SMU access or offset writes, a passing preflight, or any tuning decision |
| `vm-restart-limit` | Linux only: a NixOS VM whose `togi` is a stub that exits 2 for `run` and `restart-limit` | The module's units and scripts: three failed starts, the start limit, the fallback leave reason, the return to the normal generation, the cleared GRUB saved entry and the lock file's ownership | Any Go code, including togi's own `restart-limit` command |
| `just same` | Both trees' `tools/sim` across every dev and holdout seed of their suites, with journals compared byte for byte (above) | The session, tuner, carry and journal on the paths the suite's scenarios reach, through the simulator's SMU, host, trials and kernel | Any change to trial, detect, smu, hardware or the backends, which the simulator replaces; paths no scenario reaches |
| `just smoke` | One session per scenario of both trees' `tools/sim`, the suite's `smoke` list, compared as `just same` does | The same simulated paths as `just same`, on the sessions with the most crashes and longest histories of each scenario | Anything `just same` does not prove, and the paths the other sessions reach: run `just same` before relying on it |
| `just bench` | Simulated sessions for the chosen split (dev by default) and the model checks; with `--baseline FILE`, a comparison with that baseline by metric thresholds and, when the suite registers one, [the gate](#the-gate) | The same simulated paths as `just same` | Unchanged decisions (one changed decision with unchanged totals can pass), or anything about the packages the simulator replaces |
| `just hardware` | On the target machine, `go test -tags hardware -p 1 ./...` | Every ordinary test, plus the hardware tests: both real backends running each R1 and R2 catalog workload on core 2 in a real scope; detect reading the real kernel log; smu's driver checks, writing each core's current offset back and reading it, its BIOS context and PM table; hardware's diagnosis leaving offsets unchanged; trial's scope tests with helper backends | R3 to R7 workloads or multi-core loads on real backends, a new offset written to the SMU, the host's full preflight or ranking, or a tuning session: those need `togi doctor` or `togi run` evidence from the target machine |

## What a run records

`--out FILE` writes one JSON object per run:

- `status`: `concluded` (the session stopped after its clean cycle), `deadend`, `censored` or `error`. `censored` is a session still running when `tools/sim` reached its boot cap (`--max-boots`, default 1000; [simulating](simulating.md)): it did not conclude, and its metrics describe the partial session, so its `sim_hours` and `crashes` are lower bounds on its cost;
- `sim_hours`: simulated time from `session.start` to the last event, including the 90 s each crash reboot costs. This is the time to conclusion, or for a run that did not conclude the time it ran;
- `first_passed_cycle_h`: simulated time to the first passed cycle;
- `crashes`, `trials`, `trial_hours`, `hunts` and `combinations`;
- `passed_cycles`: completed `checking.cycle` ends marked passed, whether full or not;
- `real_answers` and `real_answer_share`: the number and fraction of completed trials answered by matching real facts; inconclusive trials and failures before workload startup count in `trials` but not as real answers;
- `scenario_real_answer_share`: real answers divided by completed trials across the scenario's selected runs, not an average of per-run fractions;
- `machine`: the fitted ensemble member selected for the seed.
- `final_profile` and `depth`, its sum;
- `hazard_per_h` and `hazard_max_per_h`: failures per hour at the final profile with every core loaded, per regime, from the simulator's own failure model. A profile that passed by luck shows up here, and no journal can show it.
- `worst_r7_hazard_per_h`, only on shared-voltage machines: the worst final-profile failures per hour across every R7 workload's full load on each CCD, each CCD's request-ordered partial chain with at least two loaded cores, and the all-core load. Each partial removes all loaded cores within 1 mV of the current highest request together, then recomputes requests with the resulting clocks and load before removing the next group. A removal leaving fewer than two cores ends that CCD's chain. Requests come directly from the shared-voltage model, and rates use the same simulator hazard as trials, without synthetic samples or random draws.

The commit, a dirty flag and the ruleset are recorded with every run.

The summary counts each scenario's runs, `concluded` runs and `censored` runs. Its time, crash and depth columns include every status, so a censored run's time and crashes enter them as lower bounds.

The summary prints each scenario's `real_answer_share` and a pooled total. Comparison scenario rows and the verdict line print the candidate and baseline shares over paired runs. These fractions describe how much of the observed path has direct real evidence, not a confidence score. Unfinished trials of a censored session have no recorded outcome and are not counted. Hazard metrics and model checks still describe the fitted fallback, not an empirical oracle hazard.

For example, `real_answer_share = 0.2` means matching real facts answered one fifth of completed trials, not one fifth of simulated hours or one fifth of the profile's risk. The rest of the path used the fitted fallback or includes completed trials that cannot count as real answers. A low share can simply mean the tuner explored profiles or load classes absent from the extract; it does not measure fit quality. Replay is enabled by the suite, not by the presence of `facts` in a machine file.

For shared-voltage runs the summary also prints `worst_r7_hazard/h`, the maximum `worst_r7_hazard_per_h` over the scenario's measured runs, and `worst_r7_runs`, their count. Legacy machines omit the JSON field and these diagnostic rows; their existing hazard metrics and reports remain unchanged.

### Scoring an intermediate profile

Bench records compute `worst_r7_hazard_per_h` only for `final_profile`, even when a run is censored. They do not score every journal profile. For an intermediate checkpoint, use that checkpoint's 16 offsets and the same machine file. Copy `tools/bench/metrics.go` into a temporary program **inside the checkout**, so Go's `internal` imports remain legal. Include its `containsDeadEnd` dependency in the program, even though scoring does not call that function. This avoids the separate-module `go.work` workaround and reuses the exact partial-chain scoring helper:

```sh
score=$(mktemp -d "$PWD/.r7-score.XXXXXX")
cp tools/bench/metrics.go "$score/metrics.go"
cat > "$score/main.go" <<'EOF'
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/shgew/togi/internal/sim"
)

func main() {
	cfg, err := sim.LoadMachine(os.Args[1])
	if err != nil {
		panic(err)
	}
	m, err := sim.New(cfg)
	if err != nil {
		panic(err)
	}
	var profile []int
	if err := json.Unmarshal([]byte(os.Args[2]), &profile); err != nil {
		panic(err)
	}
	if len(profile) != 16 {
		panic("supply all 16 offsets in core-ID order")
	}
	hazard := worstR7HazardPerH(m, profile)
	if hazard == nil {
		panic("machine has no shared-voltage R7 model")
	}
	fmt.Printf("worst_r7_hazard_per_h=%.6f\n", *hazard)
}

func containsDeadEnd(log string) bool { return strings.Contains(log, "sim: dead end ") }
EOF
go run "$score" MACHINE.toml '[OFFSET0,OFFSET1,OFFSET2,OFFSET3,OFFSET4,OFFSET5,OFFSET6,OFFSET7,OFFSET8,OFFSET9,OFFSET10,OFFSET11,OFFSET12,OFFSET13,OFFSET14,OFFSET15]'
rm -r "$score"
```

Replace `MACHINE.toml` and the JSON-array placeholders. A `profile.change` event's `to` gives the tuning profile after that change; a `trial.intent` event's `profile` gives the entire applied trial profile, which may instead be a parked hunt profile. Choose the one the checkpoint means. Score with the metric helper and machine from the same pinned simulator revision used for the comparison. This is fitted steady-state risk, without replay draws or an assertion that the checkpoint passed a cycle.

## Comparing two versions

Run the base version with `--out base.jsonl` and the candidate with `--baseline base.jsonl`. Runs pair by scenario and seed; a warning counts candidate runs without a base run, and base runs in the candidate's splits without a candidate run. For each pair that both versions concluded, the ratio is candidate `sim_hours` over base `sim_hours`; a pair either side did not conclude, censored included, has no ratio. Each scenario reports the geometric mean of its ratios with a 95% bootstrap interval. The overall ratio weights scenarios equally, and its interval resamples pairs within each scenario. The last line is a verdict:

- `REJECT`: any violation, or the interval's lower bound above 1.0;
- `ACCEPT`: no violation and the interval's upper bound below 1.0;
- `NEUTRAL`: anything else.

This generic verdict is separate from [the gate](#the-gate), which a suite registers for its ruleset and which sets the exit status.

Violations:

- **V1:** a run the base concluded no longer concludes, including a censored run.
- **V2:** a run's `hazard_max_per_h` rises by more than 0.01.
- **V3:** depth gets shallower: by more than 1 count averaged over a scenario, or by more than 5 in one run.
- **V4:** `target`, the target machine's replay-oracle ensemble, gets slower overall.

When both sides of a pair recorded `worst_r7_hazard_per_h`, comparison also reports `max_worst_r7_hazard_delta` (the largest candidate-minus-baseline difference) and `worst_r7_pairs` (the number of such pairs). Missing values are unavailable, not zero: older baselines cannot supply this comparison. This metric is diagnostic only; V2 continues to use `hazard_max_per_h`, and no verdict threshold changes.

`depth_delta` is the mean of **candidate minus baseline** profile sums over paired runs, including runs that did not conclude. Offsets are negative: a positive delta is shallower and a negative delta is deeper. For example, changing one core from −30 to −28 adds +2; changing it to −32 adds −2. It is a profile-sum difference in counts, not an average per-core offset. `crash_delta` and `max_hazard_delta` also use candidate minus baseline.

Seeds are deterministic: the same commit always produces the same runs, so rerunning cannot change a result. A change to how the tuner decides moves later sessions onto different random paths, so compare whole scenarios, not single seeds.

Record the baseline again whenever the tuner on `main`, the suite, the facts or the fitted machines change:

```sh
just bench-baseline
```

The recorded run lives at `tools/bench/baseline.jsonl`, the path the research program compares against. It includes both dev and holdout seeds. The recipe runs `just bench --split all --out` into a temporary file and commits a projection of it that leaves out `model_check` (the fitted-machine check, repeated in every run of its machine and 93% of the full file), `wall_s` (each run's real duration) and `partial_seconds` (retired); every other field stays. Comparison reads none of the three, so it prints the same against the slim file as against the full one; `--out` itself still writes the full records. Baselines recorded before the oracle ensemble do not cover `target`; do not reuse them for V4.

Compare a candidate with the recorded baseline:

```sh
just bench --split all --baseline tools/bench/baseline.jsonl
```

## Forecasting a real run

Before a hardware run, record what the simulator expects it to do, so that [`just stats` can score it afterwards](reviewing.md#scoring-a-forecast):

```sh
just forecast COPY-OF-STATE-DIR --out tools/bench/forecasts/<session>-<seq>.json
# Equivalent: go run ./tools/bench --forecast COPY-OF-STATE-DIR [--out FILE] [--suite FILE] [--keep DIR] [--jobs N] [--max-boots 1000]
```

The ensemble is the suite's `target` scenario with all its dev and holdout seeds, each run on its fitted member with real-fact replay, so new members join the forecast unchanged. For each run, the tool copies the given directory's `events.jsonl`, `state.json` and every archive entry the journal reads sessions from (archived `archive/*.jsonl` journals, the `archive/*-carry-pending` and `archive/*-compat-pending` markers of an interrupted transition, and `archive/*-reset-all` markers) into the run's own directory and resumes that copy with `tools/sim` until it concludes, as `togi run --cycles 1` would: under the configuration the copy's latest `config.loaded` recorded, with its backend store paths, durations, evidence and checking cycle, so the passes on record keep counting. A live journal of an older schema contributes only its backend store paths, on top of the default configuration ([simulating](simulating.md)). That recorded snapshot stands in for the configuration file the real machine would load, so a forecast matches the next real run only while that file is unchanged since the recorded `config.loaded`; the given directory is only read. An archive entry of those names that is not a regular file, such as a symlink, fails the forecast with an error naming it.

From a checkout on the target machine, copy those files and forecast from the copy (the sources are read-only). Take the copy while togi is not running a trial: a journal that ends inside a trial resumes, in every run, as a crash of that trial.

```sh
copy=$(mktemp -d)
trap 'rm -rf "$copy"' EXIT
mkdir "$copy/archive"
sudo sh -c 'cp /var/lib/togi/events.jsonl "$1/events.jsonl"; [ ! -f /var/lib/togi/state.json ] || cp /var/lib/togi/state.json "$1/state.json"; for entry in /var/lib/togi/archive/*.jsonl /var/lib/togi/archive/*-reset-all /var/lib/togi/archive/*-carry-pending /var/lib/togi/archive/*-compat-pending; do [ ! -e "$entry" ] || cp -P "$entry" "$1/archive/"; done' sh "$copy"
sudo chown -R "$(id -u):$(id -g)" "$copy"
just forecast "$copy" --out tools/bench/forecasts/<session>-<seq>.json
```

The anchor is the last complete event of the copied live journal: its session, sequence and time, with a SHA-256 of the session's journal lines through it. Only events after the anchor count, and the outcome ends at the run's first conclusion or dead end after the anchor: events after that point do not count. A simulated run stops there; a real run that kept checking past its conclusion, as a tuning boot does, is therefore scored by `just stats` as what a `togi run --cycles 1` run with the same history would have recorded. For each run, the forecast records:

- `status`: `concluded` (the run reached the clean cycle `togi run --cycles 1` stops at), `deadend`, or `censored` (still running at the simulator's boot cap). A run that errors fails the whole forecast;
- `hours`: from the anchor to the conclusion, or to the last event of a run that did not conclude;
- `crashes`: `crash.detected` events;
- `hunts`: hunts that ran a trial; a hunt answered only by carried facts runs none;
- `profile` and `depth`: the newest session's final profile and its sum, as in `just bench` (its checking profile, or its baseline before one exists).

The output names the anchor, the commit, the ruleset and the runs by status, then for each core's offset, the depth, hours, crashes and hunts the median, the 10th to 90th percentile and the minimum to maximum. Percentiles use the nearest rank, so each is a value some run produced; the median of an even count averages the middle two. Hours cover concluded runs only; the other rows include every run, so the crashes and hunts of runs that did not conclude enter as lower bounds. The model checks follow, as in `just bench`. Hours are simulated time, reported as is and not corrected: a simulated crash reboot takes 90 s, while real recovery took a median of 141 s on 2026-10-03, about 37 minutes over that session, so a real run takes longer than its forecast hours. The output prints this bias too.

The output and the record are identical for a fixed copy, commit, suite, machine files and facts extract: they hold no wall times or local paths. `--out FILE` writes the record as JSON: the anchor; the short commit, a dirty flag and the ruleset; `files`, each machine file and facts extract the ensemble read, by path relative to the suite and SHA-256; each run's machine, seed, split and outcome; and the summary of statuses and ranges. Name it `tools/bench/forecasts/<session>-<seq>.json` after the anchor and commit it in a pull request that merges before the run starts, so the history shows that the forecast predates the run and anyone can score it again. The record holds the anchor's identity and simulated outcomes only, no journal content.

`--split`, `--baseline` and `--same` are usage errors with `--forecast`. `--keep DIR` retains every run directory under a new directory in DIR. Without it, each successful run is deleted as it finishes; a forecast whose runs failed keeps the failed runs and prints where, and a forecast that fails after every run succeeded, such as on writing `--out`, removes its run directory and prints none.

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

The independent fits run in parallel up to `--jobs`, which defaults to the CPU count. Unconstrained fits finish before any flagged bootstrap members restart from a passing all-facts fit; those constrained refits also run in parallel. `--jobs 1` runs one fit at a time. Inside a fit, the candidates of one search step, such as the 52 limits of one core, are scored concurrently on every CPU, and in a default `just fit` the forward-chained check runs alongside the ensemble's fits; neither is bounded by `--jobs`. Concurrent scores are summed in a fixed order, so machine bytes and report order do not depend on the worker count or the CPU count.

Each bootstrap sample is fitted without constraints first. If its model check against the original extract flags a group and the all-facts fit passes that check, the fitter restarts from the all-facts fit and maximizes that **same resampled likelihood**, accepting only parameter moves that remain inside every original group's unchanged 99% interval. It does not redraw the sample, tune the seed, change the interval or repair the observed counts. This is a **checked, constrained bootstrap ensemble**: its spread is truncated to what the real facts admit, not an unconstrained bootstrap confidence interval. The fit output identifies each constrained refit and its flagged groups, including the unconstrained interval and mean probability.

If the all-facts fit itself fails the model check, there is no passing fit to restart from, so `just fit` writes every bootstrap refit as fitted, without constraints, and prints `Refit N written unconstrained` for each flagged one ([ADR 0044](adr/0044-write-flagged-target-fits.md)). The check is unchanged, and a flagged member still cannot support a target-machine claim ([ADR 0029](adr/0029-bench-verdicts-rest-on-fitted-machines.md) Decision 3).

Each generated header records the bootstrap index, the constrained groups' class, depth and observed counts, and the serialized member's model check against the original extract: `ok, no flagged groups`, or `flagged` followed by one `# Flagged:` line per flagged group with its class, loaded cores, intended duration, depth, observed counts and interval. The final model-check report checks the written files and lists the same groups with their mean probability.

Before the telemetry refresh, seed-263 refits **2 and 8** required the constraint on `R7 mprime-avx2-36k-248k-allcore`, cores 0–7, 120 s, depth -24 (`n=45`, `k=5`). Their unconstrained predictions were respectively `mean_p=0.0178977`, interval `[0,4]`, and `mean_p=0.0125439`, interval `[0,3]`. That ensemble reported `ok` on its then-current 44 eligible groups. These historical numbers do not validate regenerated machines against the refreshed evidence; use the current `just fit` model-check report.

Against the refreshed evidence, the legacy all-facts fit itself fails the model check: on `R7 mprime-avx2-36k-248k-allcore`, cores 0–7, 300 s, depth −24, it predicts `mean_p=0.0718896`, interval `[0,6]`, against 7 failures in 24 trials. The committed `target-fit-*` files are therefore unconstrained. Each member flags that group, the same workload's cores 8–15, 120 s, depth −39 group (13 failures in 38 trials), or both, and its header names which. None of the nine supports a target-machine claim until a fit passes the check, such as the shared-voltage fit [#480](https://github.com/shgew/togi/issues/480) plans.

The objective is the Bernoulli negative log likelihood of observed passes and failures, using `sim.Machine.FailureProbability` over each trial's **intended**, not measured, duration. Equal profile/class observations are aggregated without losing their counts. This includes unloaded-core failures and uses exactly the prediction the model check uses. A bounded coordinate search fits integer per-core alone/together regime limits, the shared past-limit rate and growth, the shared near-limit rate, per-core flat rates and unloaded-core idle limits. R7 failures support layered CCD shared-rail joints: each CCD starts with one all-member joint, and additional failure-profile candidates are accepted when they improve negative log likelihood by at least 0.5, up to eight layers per CCD. Joint member thresholds are fitted independently, not forced to a common depth. Workload limit overrides require at least 10 loaded trials, a failure, and the same minimum likelihood improvement. Search stops after twelve sweeps or negligible likelihood improvement; it is a local maximum-likelihood fit conditional on the selected structure, not a guarantee of a global optimum.

Each sweep also searches coupled integer limit/rate shifts. Moving active per-core, workload and idle limits by `delta` and scaling the past-limit rate by `growth^(-delta)` preserves already-past-limit hazards until a profile crosses a limit. This lets the search remove false hazards at observed clean boundaries instead of getting stuck when separate limit and rate moves each worsen the likelihood. Unsupported -50 limits stay fixed; joints are not shifted, and the same rate/limit bounds and original-evidence constraints still apply.

The positive-rate search covers past-limit rates from `1e-8` to `0.5` failures/s, near-limit rates from `1e-8` to `0.001`/s, flat rates from `1e-10` to `0.001`/s, and joint rates from `1e-7` to `0.5`/s. Growth ranges from `1.05` to `8`. Zero is also considered for past-limit, near-limit and flat rates; the joint search's zero candidate is clamped to `1e-12`/s because the simulator interprets a literal zero joint rate as the default past-limit rate. Limits range from -50 to 1, matching the simulator's stock-failure representation.

The fitter retains the original limit/joint fit, then adds a smooth loaded-CCD `[ccd]` hazard only where no fitted joint on that CCD applies. The residual stage freezes the existing joints and introduces no new joint candidates. It fits a shared log rate, nonnegative shared depth slope and two CCD effects with unit-normal shrinkage toward zero. Rate bounds are `1e-7` to `0.1`/s (zero is represented by `1e-12`/s), slope is zero or `1e-4` to `0.5` per mean depth count, and effects are bounded to [−5, 5]. Reported likelihood includes shrinkage. Existing machine files without `[ccd]` retain the original model.

The extract cannot identify every simulator parameter. Unsupported or all-passing limit boundaries stay at -50; absence of failures does not prove that boundary. The fit shares the hazard shape across cores and regimes because sparse failures cannot resolve a separate shape for each. Unsupported workload overrides stay absent; flat rates stay zero without supporting failures. Onset boost and joint delays stay at zero: decisive binary trials do not identify failure-time shapes. Crash-MCE probability, bank attribution and reset kinds retain simulator defaults; these are not parameters of the binary likelihood. Idle facts without trials are counted by the check but provide no idle exposure denominator. Idle limits can be fitted only through decisive trials with nonzero offsets on unloaded cores; otherwise they stay disabled. Offset-zero unloaded trials also constrain idle-limit candidates, since a limit of 1 adds a hazard at stock offsets. Failure-only joint activation sets identify a lower bound, not an upper bound on the rate; a saturated fitted rate is a deterministic representative on that likelihood plateau, not a precise physical measurement. Generated headers summarize the fitted parameter families, the signal mix's failure count, unsupported-limit and unobserved-idle limits, and the fixed onset, delay, MCE and reset settings.

The failure signal mix is fitted per regime, apart from the binary likelihood: which signal a failure produces does not change whether it fails, so the forward-chained check and the model check do not see it. Each fit counts the recorded signals of its sample's failures: `[model] signals` holds the counts pooled over every regime, and `[model.regime_signals]` the counts of each regime with failures. Counts are the maximum-likelihood weights, so a signal the sample never recorded gets no weight, and a regime without failures draws from the pooled counts. An uncorrected machine check counts as a crash, which the simulator draws as a crash that leaves an MCE with probability `crash_mce`; a failure without a recorded signal is not counted. A sample without any recorded failure signal, such as a bootstrap resample without failures, keeps the simulator's default weights and writes neither table. Fitted joints and the `[ccd]` residual carry no signal of their own, so their R7 failures draw from the R7 counts, or the pooled counts when the sample has no R7 failures, instead of always crashing. The shared-voltage anchor fits the same counts from all decisive trials.

Generation prints likelihoods, CCD joint parameters, the fitted signal counts per regime with the number of failures behind them, model checks against the **original** extract (also for bootstrap fits), and elapsed wall time. Files contain no timestamps and use stable ordering and full-precision parameters, so fixed inputs and seed reproduce them byte for byte on one architecture. Between amd64 and arm64 they can differ in the last digits: Go implements its math functions separately for each architecture, and the arm64 compiler fuses multiply-adds. A last-digit difference after refitting on the other architecture is not a model change. A model-check flag is not silently repaired or excluded: inspect the named class, depth and interval before using that ensemble member as target evidence. A flag may reflect a poor local optimum, the current model's shared-shape/joint assumptions, or a sparse failure absent from a bootstrap sample; it is not by itself proof of an impossible fit. The shared `tools/modelcheck` implementation is used by both `just fit` and `just bench`, with the same 99% intervals.

### In-sample shared-voltage anchor

```sh
just fit-shared-voltage
# Optional: --facts EXTRACT.jsonl.gz --out DIRECTORY
# Equivalent: go run ./tools/fit --shared-voltage-in-sample
```

This opt-in mode fits **all decisive trials**, including record-only facts, to the 16-core shared-voltage model and writes only `target-shared-voltage.toml`. It requires one nonempty BIOS context and 16-core profiles. It leaves the default `just fit`, `just forward`, bootstrap dispatch and `target-fit-*` files unchanged. The anchor mode rejects `--bootstrap`, `--seed`, `--forward-only` and `--seal`, even when their supplied values equal their defaults.

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

The generated header explicitly says **IN-SAMPLE, NOT forward-validated**, records the failed 2026-10-04 [#306](https://github.com/shgew/togi/issues/306#issuecomment-5979625589) bar, and declares relative `facts` and `[bios_context]`. Workloads and cores have stable order; floating-point parameters have full precision. Reports print raw total and per-trial in-sample Bernoulli log loss for all trials and R7, and predicted/observed R7 counts **for record-only purposes**, not as held-out scores. These diagnostics also print for a flagged fit before failure returns without writing an anchor. Fixed inputs reproduce files and reports on one architecture, excluding the elapsed line.

**Q17 forward bar remains authoritative.** A shared-voltage fit used for forward claims or regeneration of `target-fit-*` must beat the constant predictor's log loss on a held-out session's R7 trials and predict total R7 failures within a factor of 2 ([#105](https://github.com/shgew/togi/issues/105)). The first candidate failed on 2026-10-04: 36.0 predicted against 17 observed (above the allowed 34), despite log loss 0.270 below constant 0.399. That check was not blind; it was not retuned afterwards, and a future forward claim needs a new blind session. This all-facts anchor neither reruns nor supersedes that check and does not authorize `target-fit-*` regeneration.

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

Machine paths resolve relative to the suite; facts resolve relative to the machine. The declared facts and BIOS context let `trialfacts.Extracts.Replay` construct `sim.NewReplay` for matching decisive trials, with fitted fallback on unmatched classes/profiles. The bench process reads each extract once and shares it between its metric replay oracles and the model checks of every member that declares it; each simulated session's simulator still loads the extract itself. A replay run remains in-sample evidence, not forward validation.

The suite includes the hand-set `shared-voltage`, the `target-shared-voltage` anchor and two R7 adversaries, each with dev seeds 1–12 and holdout seeds 101–112. Both adversaries passed the unchanged model check on all 52 eligible groups. Under ruleset 8, their dev and holdout median `worst_r7_hazard_per_h` exceeded the corresponding unmodified anchor medians by at least 1.5×. `target-r7-vf-boost` changes only AVX2 core 5's required-clock coefficient to 0.06 V/100 MHz; `target-r7-request-gap` changes AVX-512 cores 9–11's required-clock coefficients to 0.06 V/100 MHz and their required-voltage thresholds to 1.18 V, except core 10 at 1.22 V. Request, clock and power parameters stay at the anchor's values. Across measured R7 trials, both machines reproduce the anchor's loaded requests exactly, retaining its residuals against recorded requests. These expose record-only partial failures and untested request-ordered partial loads, not forward-validated hardware failure rates.

### Forward-chained check

After the ensemble's model check, `just fit` orders sessions with decisive trials by their UTC session IDs, comparing equal-second numeric suffixes numerically (`-2` before `-10`). For each session after the first, it fits all earlier sessions' decisive trials once, without bootstrap resampling or model-check constraints, then predicts only the held-out session. These fits stay in memory and do not change the generated machine files.

The prefix fits run in parallel with the same `--jobs` limit; in `just fit` they run alongside the ensemble's fits, and the report prints after the ensemble's. Scoring and pooling remain in session order, so no held-out outcomes enter their own training prefix and the report is unchanged apart from elapsed times.

Each session row names the session ID, ruleset and `training_sessions` count. `trials` and `failures` count held-out decisive trials and observed failures; `predicted` sums the fit's failure probabilities over their intended durations. `log_loss/trial` is the mean Bernoulli log loss, with probabilities clamped to `[1e-4, 1-1e-4]`. It compares the `fit` to a `constant` predictor whose probability, shown in parentheses, is the earlier training trials' failure rate. Lower loss is better: fit loss above the constant means the fit predicts that later session worse than a single average failure rate.

`failures_p<0.01` counts observed failures assigned less than 1% probability: outcomes the model treated as nearly impossible. `exact_matches` is the number and share of held-out trials whose trial class and full profile occur in the earlier training trials. That share bounds what replay could answer from earlier evidence; it does not establish that replay's answers would be correct. `flagged/eligible` counts held-out groups flagged by the unchanged model check, with at least 10 trials and the same 99% binomial intervals. The indented regime rows also show fit and constant log loss over the same regime's trials, using exactly the existing probability clamp and each session's prefix-wide constant.

`Pooled` sums all held-out sessions' counts and predictions and divides summed log losses by their total trials. Its constant uses each session's own earlier prefix, not one failure rate fitted to the pooled outcomes; its group counts sum the separate held-out checks. `Forward-chained elapsed` reports the check's own wall time, which in `just fit` overlaps the ensemble's fits. This is evidence about extrapolation to later sessions, not a gate or a guarantee about unseen profiles or other machines.

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
