# Benchmarking

`togi-tool-bench` (`just bench`) runs a fixed suite of simulated sessions and compares two versions of togi. Use it to show that a tuner change makes sessions conclude sooner, or no later, without giving up depth or leaving a profile that fails. Like `togi-tool-sim`, it is a development program and is not shipped.

Autonomous research follows [Research campaigns](#research-campaigns): its modes, limits, prohibitions, record and end state, then the mode's program and the issue's brief. [ADR 0029](adr/0029-bench-verdicts-rest-on-fitted-machines.md) records why target claims require checked fits and real-fact replay.

```sh
just bench [--split dev|holdout|all] [--out FILE] [--baseline FILE] [--keep DIR] [--jobs N] [--max-boots 1000]
```

## The suite

`tools/bench/suite.json` lists scenarios in a top-level `scenarios` array. Each scenario names a simulator `machine` JSON file in `tools/bench/machines/` (none means the seeded default machine), or an ensemble in `machines`, and two sets of seeds, `dev` and `holdout`. Ensemble seeds select successive members round-robin by their position within each split, not by the seed value. `"replay": true` enables the same-BIOS replay oracle over the selected fitted member:

|Scenario|Models|
|---|---|
|`default`|the seeded default machine|
|`target`|real-fact replay over the all-facts target fit and eight bootstrap refits, all flagged by the model check and the refits unconstrained ([ADR 0044](adr/0044-write-flagged-target-fits.md)), including CCD0 and CCD1 R7 joints|
|`flat-hazard`|rare failures at any nonzero offset on two cores (issue #105)|
|`late-onset`|R7 joint failures that start only after four minutes of load; the cores' own limits have no delay|
|`idle-limit`|cores that fail idle at shallower offsets than under load (issue #106)|
|`misleading-mce`|joint crashes that leave an MCE naming one core (issue #114)|
|`target-nonmember-mce`|synthetic conclusion guard of the archived Ruleset 11 gate, not an admitted adversary and with no adversary admission: CCD0 joint crashes deliberately name nonmember core 15; parked hunts can stop at that core's parked offset of zero (related to issue #114). It makes no hazard or time claim.|
|`shared-voltage`|hand-set shared-rail R7 model with workload-dependent requests, clocks and per-core voltage demand; no facts or replay|
|`target-shared-voltage`|real-fact replay over the all-facts shared-voltage anchor; an in-sample fit, not forward-validated|
|`target-r7-vf-boost`|overlay of the anchor: AVX2 core-5 required voltage rises faster than its request as partial CCD0 loads boost; in-sample, not forward-validated|
|`target-r7-request-gap`|overlay of the anchor: AVX-512 required-voltage curves leave core 11 dependent on core 10's rail support: full CCD1 loads pass but partial loads without core 10 can fail; requests unchanged; in-sample, not forward-validated|

The failure sources are separate from the signals they produce:

| Scenarios | Failure sources |
|---|---|
| `default` | Seeded loaded-core limits. No unloaded-core hazard or joint. |
| `flat-hazard`, `idle-limit`, `late-onset`, `misleading-mce` | Loaded-core limits, plus respectively nonzero-offset flat hazards on loaded or unloaded cores, unloaded-core idle limits, delayed R7 joints, or R7 joints with misleading MCE attribution. |
| `target` | Exact real-fact answers where matched; otherwise fitted loaded-core limits, any fitted flat hazards, R7 joints and the residual `[ccd]` hazard where no joint applies. |
| `target-nonmember-mce` | Exact real-fact answers where matched; otherwise a frozen copy of an older target-fit-4 limit/joint fit whose CCD0 joint crashes name nonmember core 15. Synthetic, not tracked against the current fit. |
| `shared-voltage` | Loaded-core voltage-margin hazards for multi-core R7; loaded-core limits elsewhere. No replay, unloaded-core hazard or platform background rate. |
| `target-shared-voltage`, `target-r7-vf-boost`, `target-r7-request-gap` | Exact real-fact answers where matched; otherwise loaded-core voltage-margin hazards and a CO-independent platform background rate for multi-core R7, and fitted loaded-core limits elsewhere. No unloaded-core hazard. The two adversaries are overlays of the anchor and inherit everything else from it. |

On `target-shared-voltage`, changing only unloaded cores' offsets cannot change the fitted multi-core R7 hazard. All `flat` rates are zero and no core has an `idle` limit; the voltage model computes power, clocks and the rail from loaded cores only. The same is true of its two shared-voltage adversaries. Parking unloaded cores can still change which real facts match, since replay matches the **whole** profile. A failure during a parked locate on this anchor is therefore not proof of an unloaded-core voltage dependency: it can come from a loaded core, the background rate or replay. This is a limitation of these scenarios, not a statement about hardware ([simulator model](simulating.md#shared-voltage-r7-model)).

Iterate on `dev`. Run `holdout` only to confirm a result, so the holdout seeds stay unseen by the change being tuned.

### The gate

A ruleset's bench gate is the `gate` object in [`tools/bench/suite.json`](../tools/bench/suite.json), committed before the ruleset is scored. It names the gate `id`, the `baseline` it is judged against (the ruleset and the commits that recorded the runs; a pull request that re-records runs adds its commit), the seed `split`, the `gated` scenarios, the `pooled` subset, the `conclude` scenarios, the `quantiles`, the bootstrap (`confidence`, `resamples`, `bootstrap_seed`) and `max_time_ratio`. The object and its `note` are the definition and the rationale; this page does not restate them per ruleset. The gate comes from [issue #491](https://github.com/shgew/togi/issues/491) and [issue #570](https://github.com/shgew/togi/issues/570).

The legacy session suite has no current gate. Ruleset 13 registers a separate [day/night gate](#daynight-gate) because a session's conclusion cannot measure a profile's daily-use safety.

A gate's `notes` array preserves accompanying provenance and caveats as JSON data; it does not affect scoring. `just bench` prints each `notes` entry of a registered gate as a `suite: ...` line before its summary.

With `--baseline`, `just bench` judges the gate after the generic verdict below, prints each criterion with its estimate, interval, threshold and `PASS` or `FAIL`, then an overall `gate ID: PASS` or `FAIL`, and exits 0 only when the gate passes. Run it as `just bench --split all --baseline tools/bench/baseline.jsonl`, scored once on every seed of the gate's split; a different `--split`, a candidate or baseline run missing for a required seed, a baseline recorded at another ruleset or commit than the gate names, or a missing `worst_r7_hazard_per_h` fails the gate with a refusal that names the cause. A suite without a `gate` prints no gate and exits as before. The criteria, per scenario unless noted:

- **Conclusion:** every seed the baseline concluded is still concluded by the candidate, on each gated and each `conclude` scenario.
- **Median and 90th percentile** (linear interpolation) of the final-profile `worst_r7_hazard_per_h`: fails only when the `confidence` interval of the change lies entirely above 0. The change is the candidate's quantile minus the baseline's, not the quantile of per-pair differences. The interval resamples seed pairs `resamples` times, drawing from a `StdRng` seeded with `bootstrap_seed` ([Judging the gate](#judging-the-gate)), so a run always prints the same interval. Over 24 seeds the interval is wide, so the 90th-percentile criterion has low power: it catches a large tail rise, not a small one.
- **Time:** the geometric mean over pairs both concluded of candidate `sim_hours` over baseline's is at most `max_time_ratio`, with no exemption.
- **Pooled median:** the median over the `pooled` scenarios' seeds together is strictly lower than the baseline's; equal fails.

Maxima and everything else are reported by the summary and the comparison, not gated. Treat a failed gate as the owner's decision, not a threshold to renegotiate after scoring.

### Day/night gate

[`tools/bench/day-night.json`](../tools/bench/day-night.json) is the Ruleset 13 registration for [#1216](https://github.com/shgew/togi/issues/1216). It is committed before any scoring. It freezes 200 development seeds and 200 disjoint holdout seeds, 730 calendar days, eight active daily hours, three eight-hour nights a week, the scenario set, incremental hazard budget, thresholds and tail treatment. Iterate on development seeds; reserve holdout for the real policy's once-only confirmation, separately at the fallback slopes 1.5× and 2×. Neither comparator is the real policy, and comparator results cannot pass this gate.

```sh
just bench-day-night --split dev
just bench-day-night --split dev --out day-night.jsonl
```

The command runs **CO 0 forever** and **fixed margin** over every registered scenario. Output is a per-scenario metrics table; `--out` preserves per-seed metrics as JSON Lines. This experiment is in memory, independent of the legacy session suite and baseline. It does not run hardware. [Simulating](simulating.md#calendar-daynight-experiments) owns the truth, calendar, policy seam and comparator behavior.

True safe uses the hazard **CO adds over stock**, not total failures: λ* = 1/3,000 active hours jointly and λ*/CCD-count on each CCD. It is the deepest admissible profile in the write range; both-CCD interaction risk must satisfy the joint bound too. The reference repeats the ordered eight-hour daily mix and sixteen-hour sleep at periodic thermal equilibrium, integrating hidden classes, delayed onsets and intended durations. Policy-dependent night heating changes actual simulated risk, not this reference target. Total depth wins, then balanced CCD depth, then lexicographically smaller depths break ties. Only metrics can inspect the oracle; policies see machine-seam observations, never true safe.

The table distinguishes true CO-caused daily crashes from CPU-suspect classifications, which can include wrong or missing answers to non-CPU unclean ends. Per-CCD metrics retain first-year crashes, convergence day, maximum overshoot, cap-promotion steps, first converged and final depth, and counts retreated on background alone versus new CO evidence. Sleep is wall time only, not daily or trial exposure. A moving-cap scenario reports steps separately; its count does not enter the fixed-cap bound. A firmware change invalidates prior qualification and returns day offsets to stock.

The registration is authoritative: covered-class climb is crash-free in at least 90% of seeds; hidden-class first-year crashes are at most two per CCD in at least 90%; maximum overshoot is at most one count in at least 95% and never more than three; median days within one count of true safe is at most 90; after two years on the stock-background scenario at least 90% keep converged depth minus one; fixed caps need at most seven promotions per CCD. Each criterion is evaluated per scenario using each seed's worst CCD, never pooled. Every censored or nonconverged run stays in the denominator, with infinite convergence time; partial counts cannot earn a pass. The committed registration describes applicable scenarios and exact empirical fractions. A failed score is an owner decision, not a reason to relax thresholds.

### Historical gates

The Ruleset 9 gate is defined by [ADR 0038](adr/0038-self-sufficient-cores.md) and [#105](https://github.com/shgew/togi/issues/105); [ADR 0040](adr/0040-located-hunts.md) records the Ruleset 10 gate. The [Ruleset 11 archive](../tools/bench/gates/ruleset-11/) holds its suite, baseline, candidate and scored report. These are historical records, not current-anchor measurements. Archived Go gates are never re-scored by the current bench ([Judging the gate](#judging-the-gate)).

The [#493 measurement record](https://github.com/shgew/togi/issues/493#issuecomment-6040930168) identifies the tuner, simulator and regenerated anchor used for its comparison. Earlier measurements of `target-r7-vf-boost` and `target-r7-request-gap` describe frozen copies, not today's overlays. See [reproducing an older ruleset](#reproducing-an-older-ruleset) before comparing historical builds.

A scenario may also list `smoke` seeds, each one of its dev or holdout seeds: the sessions `just smoke` compares ([below](#proving-unchanged-decisions)). The list holds one session per scenario, the one with the most crashes and the longest history that takes about two seconds of CPU or less on the current simulator, so it changes in a reviewed pull request, never silently.

`--suite FILE` runs another scenario file instead, with machine paths relative to it. The [adversarial search](../tools/bench/adversary.md) scores candidate machines this way and turns confirmed findings into new scenarios.

The target scenario has 18 dev and 18 holdout seeds: two seeds per ensemble member in each split. Its oracle draws uniformly from decisive facts matching the entire applied profile and trial class (regime, workload, sorted loaded cores and intended duration). Draws are deterministic per seed and trial ID/index. Condition, phase, source ruleset and evidence epoch do not restrict a class match; BIOS context does. The oracle never uses facts from another BIOS context or records without one. Non-matching trials and trial-less failures use the fitted machine. The tuner sees only the resulting journal evidence, not the oracle or its extract.

Every run is a `togi-tool-sim` subprocess with its own state directory, in parallel up to `--jobs`. `--max-boots` bounds each session by simulated work, not wall time: a session still running after that many simulated boots stops with `censored` status ([simulating](simulating.md)), so a slow or loaded host makes a run take longer but never changes its status. `--timeout` (default 30m) is only a safety net against a hung simulator: a run that exceeds it is killed and the command fails with a harness error naming the session, never recording a session outcome. Runs start longest first, by the wall times recorded by earlier runs in the user cache directory (`togi/bench/costs.json` under `$XDG_CACHE_HOME` or `~/.cache` on Linux, `~/Library/Caches` on macOS), and in suite order for sessions without a record. The first failed run cancels the rest. SIGINT and SIGTERM kill the simulators and remove the temporary directories. `--keep DIR` keeps the state directories under a new `DIR/bench-*/<scenario>/<split>-<seed>` (the path is printed), so the read-only commands can inspect a run with `--state-dir`. Without `--keep`, each run's state directory is removed as soon as its metrics are read, so a full-suite run holds at most `--jobs` of them at once.

Each simulator subprocess retains parsed events across its simulated reboots and writes `state.json` only at stop, avoiding journal re-parsing and per-event state-file rewrites. It still appends every event to `events.jsonl`; `--keep` leaves the journal and final state available for inspection. The parent reads and parses each journal after the child exits, and children are not run in memory or made to report a digest ([ADR 0060](adr/0060-bench-children-keep-temporary-journals.md), [#613](https://github.com/shgew/togi/issues/613)). This changes wall-clock overhead, not simulated durations or tuner decisions. Recovery and interruption tests use the file-backed path, and fixed-seed equivalence tests compare both output files byte for byte on the default machine and `target-fit-0.json`.

Trial samples stay in memory; retained sessions have no `trials/<trial-id>/samples.jsonl`. Their journal evidence, final state and read-only dashboards are unchanged. For per-second sample files, run `togi-tool-sim` with `--samples` ([simulating](simulating.md)).

### Running one scenario

There is no `--scenario` flag. Write a scratch suite containing just the scenario, with an absolute machine path so the suite may live outside the checkout. From the checkout, in the dev shell:

```sh
scratch=$(mktemp -d)
repo=$(pwd -P)
cat > "$scratch/suite.json" <<EOF
{"scenarios": [{
  "name": "target-shared-voltage",
  "machine": "$repo/tools/bench/machines/target-shared-voltage.json",
  "replay": true,
  "dev": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
  "holdout": [101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112]
}]}
EOF
just bench --suite "$scratch/suite.json" --split all \
  --keep "$scratch/runs" --out "$scratch/results.jsonl" --timeout 900
```

Keep `scratch` until inspection is finished. To select another scenario, copy its seed lists and `replay` setting from `tools/bench/suite.json`. For an ensemble, use `"machines": ["ABSOLUTE-MEMBER-0.json", "ABSOLUTE-MEMBER-1.json"]` instead of `"machine"`. Facts paths still resolve relative to each machine file.

### Reproducing an older ruleset

**Historical.** Rulesets 9 and 10 ran on the Go build. The paths below (`internal/sim`, `tools/sim`, `internal/simrun`, `.toml` machines) exist only at those revisions, and the recipe runs that revision's own tools in its own dev shell. No current crate reproduces these runs: the current simulator's seeds and streams differ ([Simulating](simulating.md#the-simulator-togi-sim)).

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

The diff must be empty. In that detached worktree, enter the dev shell and run the one-scenario recipe above, written in that revision's TOML suite and machine format (its machine is the restored `.toml` file). For ruleset 10, use a detached worktree at `645f529` without the port. Both runs must use the same scenario name, seeds, machine and replay setting. Preserve the worktrees and scratch results until analysis ends; their diffs explain the deliberately dirty ruleset-9 records.

For a comparison on a later simulator, review **every** commit that changed the simulator paths since the older ruleset, not just those two signal-mix commits:

```sh
git log --reverse --no-merges --format='%h %s' 84c2a01..origin/main -- \
  internal/sim tools/sim
git log --oneline 645f529..origin/main -- internal/sim tools/sim
```

The two-commit port is not sufficient for main at `2f318490` (2026-10-10). Later changes couple the simulator to `internal/simrun`, session recovery and journal buffering. `0ea69fc1` also changes samples from maps to `machine.PerCore`, with corresponding changes in requests and session code. Copying only `internal/sim` and `tools/sim` then fails to build against the older infrastructure. A current-simulator port must carry those dependencies without importing newer tuning decisions, and still leave the simulator, machine and facts diff empty against its pinned reference. A generic current-main port procedure moves to [the redesign, #493](https://github.com/shgew/togi/issues/493): this needs an infrastructure compatibility boundary, not a guessed cherry-pick list. The pinned recipe above reproduces the documented measurements.

## Auditing journals

Every run `just bench` simulates is audited before its state directory is removed ([below](#the-bench-togi-tool-bench)). `just audit` audits state directories it is given, such as a copy of a real one or a run `just bench --keep DIR` kept:

```sh
just audit COPY-OF-STATE-DIR OTHER-STATE-DIR   # audit of current and archived journals
```

### The audit (`togi-tool-audit`)

The checks are one library, `togi-tool-audit`, which the bench, `togi-tool-sim suite` and the `togi-tool-audit` command share. `just audit` runs the command:

- Each directory is audited as a run that may still be in progress, so its live journal need not end in a conclusion and a `state.json` behind its last event is not compared.
- The report is JSON Lines on stdout, one object per violation with `directory`, `journal` (`live` or an archived session id), `seq`, `check` and `reason`; `check` is a journal check of [Invariants](spec/tuner.md#invariants), or `unreadable` (a directory that cannot be opened or a journal that cannot be read, with neither `journal` nor `seq`). The summary line `directories= journals= violations=` goes to stderr. Exit status 1 means a violation, 2 a usage error, such as no directory.
- The audit takes each directory's writer lock, so a directory a tuner is running is refused. Taking the lock creates the directory's `lock` file and sets its mode to 0711, so audit a copy.
- The audit reads journals leniently ([Journal](spec/journal.md)): an event of a kind this build does not know is left out, a cause that names it is not a bad cause and the gap it leaves is not a sequence violation. Each journal that lost anything is named on stderr (`togi-tool-audit: "DIR" JOURNAL: ...`) before the summary; stdout and the exit status are unchanged.

Before [#546](https://github.com/shgew/togi/issues/546), `just audit` without directories swept every scenario over 200 seeds in a bench subprocess of its own and flagged wall-time outliers and boot-cap runs; the bench's own audit replaces the sweep, and a censored run is no longer a violation.

The audit checks recorded failure-point and combination avoidance independently of the tuner, SMU intent/write/readback ordering, the offset range ([Invariants](spec/tuner.md#invariants); readbacks and baselines report the hardware and are not bounded), causal references, the conclusion of a finished simulated run, and exact `state.json` replay when the snapshot's `last_seq` matches the replayed journal. A multi-core R7 failure naming a core at CO 0 records a failure point only when a dead end cites it ([R7 request order and attribution](spec/tuner.md#r7-request-order-and-attribution)). A snapshot at another sequence is stale and is skipped: a crash between the journal fsync and the state write can leave it behind by design. A session from another ruleset has no current projection and is skipped too. So is a session started by an older journal schema: replay always stamps the current schema, and the next run archives that session. Likewise, a `state.rebuilt` event is a violation only when its differing `fields` omit `last_seq`, meaning a current snapshot disagreed with replay, and the run's next `config.loaded` records the same build as the previous one; a changed build may project fields an older snapshot lacks, which is a legitimate upgrade. A rebuild whose run ends without a `config.loaded` (its shutdown, dead end, session archive or a later boot, or the end of a finished simulated journal) is a violation too; one at the end of a live journal is deferred. Real journals may still be live, including between a write and its readback. A write followed by a reboot or the end of an archive is an interrupted action, and a same-boot resume's readbacks of every core complete it; archived transitions and reset boundaries are valid session endings. Audit copies of real state directories so the journal and state projection are a consistent snapshot.

## Proving unchanged decisions

For a shape-only change, `just same [base]` proves that decisions are unchanged on simulated paths only: the simulator stands in for `togi-trial`, `togi-detect`, `togi-smu`, `togi-hardware` and the backends, so it proves nothing about a change to them ([what each check proves](#what-each-check-proves)). The base defaults to `origin/main` and is exported locally without fetching or registering a worktree. It builds the base and current simulators with `cargo build --release --locked` and runs each tree's own suite and machine files across every dev and holdout seed. Sessions pair by scenario, split and seed. Every current and archived journal pairs by its relative path and is compared byte for byte, after removing only `version`, `rev` and the exact build description in `msg` from `session.start` and `config.loaded`. Everything else, including `ruleset`, `schema`, `fixes` and `evidence_epoch`, must match.

Each side's run is reduced to a digest of its normalized journals and its exit status, which the command caches in the user cache directory (`togi/bench` under `$XDG_CACHE_HOME` or `~/.cache` on Linux, `~/Library/Caches` on macOS) under a key made of the simulator binary's SHA-256, `--max-boots` and the sessions with the machine and facts files they read. The binary carries the code, the toolchain and the platform, so a base that does not change the simulator is a cache hit across commits, and a rerun after an edit that leaves the simulator unchanged (a comment, a doc, a test) runs nothing. When both trees build the same simulator and read the same inputs, the command says so and reports no difference without running any session. Every other session runs once per side that lacks a record, longest first, as independent jobs, and each run directory is removed as soon as its digest is taken and its pair compared. Only the cache keeps what a run proved; the cache keeps the eight most recently used keys.

A difference stops the comparison: the remaining runs are cancelled and the first difference prints. `--keep-going` runs every session and counts every difference. A differing session prints its first differing event's journal path, line number and both normalized lines; its run directories are retained and their directory printed on stderr, after rerunning any side that was cached or already deleted. Missing sessions or journal files and differing simulator exit codes also count as differences. The command exits 0 when all sessions match and nonzero on a difference, execution error (a simulator that exceeds the hang safety limit included) or invalid arguments. A difference is fixed or split into its own issue, never explained away. A session that differs only because of a stale cache entry shows `<journal digest>`; rerun with `--no-cache` to rule that out.

Every session's wall time is part of its cached record, so the report compares head with base even when the base ran in an earlier invocation. It prints the total of both sides and every session whose head took more than twice its base's time. That is informational: it never changes the verdict or the exit status.

`just smoke [base]` is the same comparison, with the same cache, fail-fast and flags, over only the sessions the suite lists under `smoke`: a quick check while editing, to run `just same` after. A session the base suite lacks counts as a difference.

This is not a CI check: it needs two builds. `just bench` cannot prove equality: only with `--baseline FILE` does it compare metrics against thresholds, and one changed decision with unchanged totals can pass.

To control concurrency, caching or retention directly, run `togi-tool-bench same` with its own flags, described [below](#the-same-command).

## The suite gate

The release's dispatched full check runs every session of `tools/bench/suite.json`, dev and holdout alike, on the simulator (`togi-tool-sim`, [Simulating](simulating.md#the-simulator-command-togi-tool-sim)) as the flake checks `sim-suite-0` to `sim-suite-<N-1>`; pull request CI does not ([ADR 0074](adr/0074-pull-request-ci-gates-landing-on-main.md)). `nix/sim-suite.json` declares N. Each check builds the release `togi-tool-sim`, runs its shard of the suite (the partition gives shard sizes that differ by at most one) with one session per core, and then checks its manifest against exactly the run ids the partition gives that shard. A session passes only when it is `clean`:

- it stopped for its clean cycle: `shutdown` reason `cycles`, never a dead end, a signal or the 1000-boot cap;
- its resume state matched a fresh fold of the journal after every crash and at the end;
- `togi_tuner::replay` folds its whole journal;
- `togi-tool-audit` finds no violation in any journal of its state directory, and its published `state.json` equals a fresh fold of the live journal.

In full mode, the aggregating `check` job reads N from `nix/sim-suite.json` and fails unless the selected checks include exactly `sim-suite-0` to `sim-suite-<N-1>`, whether cached or built, so a deleted or renamed shard cannot silently pass. The suite declares no expected dead end, and none is to be invented to pass the gate. `just sim-suite` runs the same suite locally in one shard and checks it; [`togi-tool-sim`](simulating.md#the-simulator-command-togi-tool-sim) also has a test that splits one session across processes by `--max-boots` and requires the same journal and `state.json` as an uninterrupted run.

`just gate`, `just push-fix` and `just check` on Linux run the shards, even without KVM. `just sim-suite` reproduces the whole suite, and `just check-one sim-suite-<i>` reproduces shard `i`. Raise N in `nix/sim-suite.json` whenever a shard's full-check job (`completedAt − startedAt` in `gh run view --json jobs`) is slower than the slowest job of the same run that is not a shard. That keeps the suite within [#627](https://github.com/shgew/togi/issues/627)'s limit that a job is never slower than the current slowest one. The `build` job's `timeout-minutes: 45` stops a session that never concludes, instead of holding a runner for six hours.

This gate is separate from `just bench`: it proves that every session concludes cleanly, not that a tuner change concludes sooner or keeps its depth.

## What each check proves

Cite a check as evidence only for the packages it exercises. `just gate` covers pull request CI's checks except `package` and cargo-deny's advisory audit, plus the simulator shards on Linux and the VM tests when `/dev/kvm` is writable. `just push-fix` also builds those heavy checks. `just check` runs every flake check the host builds; the release's dispatched full check runs every Linux check, including `trial-scope-tests`, `vm-recovery-tests`, `vm`, `vm-host-lock`, `vm-trial-scope`, `vm-restart-limit` and `vm-emergency-stop`.

| Check | Runs | Exercises | Does not prove |
|---|---|---|---|
| `just test`, `just focus` | `cargo nextest run` on the workspace or the named crate | Every crate's ordinary tests: the tuner, session, carry and journal directly, and trial, detect, smu, hardware and the backends against fakes, fake file trees and fake package trees | That a stress program accepts its generated configuration; real systemd scopes, kernel logs or SMU access |
| `board-test`, `just board-test` | Nushell assertions with fixed GitHub JSON and time, in `just gate` and the flake check | Coordination-board readiness, grouping, links, overlaps, interaction expiry, failures and golden text | Live GitHub API availability or authentication, any togi runtime code or hardware |
| `sql-test`, `just sql-test` | DuckDB over fixed state directories, in `just gate` and the flake check | The `just sql` views: current and archived journals, plain and gzip samples with the plain file preferred, session-scoped trial joins, a torn tail that does not parse, a directory without archives or samples, reading the views again after a trial's samples were compressed, and refusing a trial directory the user cannot list | That a real or simulated journal's fields still match the views, any togi runtime code or hardware |
| `doc-links` | `lychee --offline --include-fragments` over every Markdown file, in `just gate`, `just push-fix` and the flake check | That each relative link names a file in the repository and each `#fragment` a heading of the file it names | That a web link resolves, or that a link says what its text claims |
| `module` | Evaluates `nix/module.nix` for x86_64-linux on any host (`nix/module-test.nix`), with the package built with the revision `dev` (`package`'s `package`), which the evaluation references without building it | The module's options and generated configuration: GRUB mirror constraints, the grubenv mount dependency, the tuning specialisation's 30 s watchdog and kernel parameters, the restart-limit and leave-tuning-boot scripts, the lock file rule and package overrides | Any code of togi, or that the configuration boots and behaves as evaluated |
| `cargo-deps` | crane's `buildDepsOnly`: the locked dependencies of every workspace crate in the dev profile, the artifact `clippy`, `nextest-<group>`, `doc`, `changes`, `trial-scope-tests` and `vm-recovery-tests` start from | That the locked dependencies build; its own job pushes the artifact to Cachix, since no other check's output references it | Any workspace crate's code or tests |
| `trial-scope-tests` | Linux only: builds `togi-trial`'s `hardware` and `cached_errors` test binaries with the `hardware` feature, as `togi-trial-tests` and `togi-trial-cached-errors` | That those tests compile | Anything at run time: `vm` and `vm-trial-scope` run the binaries |
| `vm` | Linux only: a NixOS VM with the module, whose `togi` is the package built with the revision `dev`, and its tuning specialisation (`nix/vm-test.nix`) | `togi-trial-tests`'s `hardware::` tests, except the helper process and the R1 and R2 workload test, as root under real systemd and its pin test as a delegated user, with helper backends; the installed `togi`'s dead end at preflight on the tuning boot and the saved entry it clears; a held host lock stopping `togi.service` with exit 3, no restart and the boot, saved entry and state unchanged; the watch and console services; booting the GRUB tuning entry; the armed watchdog's reset when PID 1 freezes | Real stress programs, SMU access or offset writes, a passing preflight, or any tuning decision |
| `vm-host-lock` | Linux only: a NixOS VM with the module and a hardware-test group, booted normally, whose `togi` is the package built with the revision `dev` (`nix/vm-test.nix`) | `nix/provision-lock.py`, the manual-install provisioning of [the how-to](howto.md#host-lock-and-delegated-hardware-tests), on fresh, legacy, held and symlinked locks; the provisioned lock denying an unrelated user; the installed `togi`'s refusals of a foreign-owned lock, a read-only lock and a held lock, by exit status, inode, mode and content | The wording of any refusal, `togi-trial-tests`, or the tuning boot |
| `vm-trial-scope` | Linux only: a NixOS VM with the module's tuning node, running `togi-trial-tests` and `togi-trial-cached-errors` as root and as a delegated user | The scope tests that need real systemd, as root and, for the delegated subset and `cached_errors`, through the user manager, with helper backends, leaving no `togi-*` scope behind | Real stress programs or SMU access, or the installed `togi` |
| `vm-restart-limit` | Linux only: a NixOS VM whose `togi` is a stub that exits 2 for `run` and `restart-limit` | The module's units and scripts: three failed starts, the start limit, the fallback leave reason, the return to the normal generation, the cleared GRUB saved entry and the lock file's ownership | Any code of togi itself, including its own `restart-limit` command |
| `vm-recovery-tests` | Linux only: builds `togi`'s `vm_recovery` integration-test executable as `togi-vm-recovery` | That the production CLI recovery path and its persistent-fake-machine fixture compile | Runtime recovery or real SMU access |
| `vm-emergency-stop` | Linux only: a NixOS VM using the module's service, the production emergency-stop CLI on a persistent fake SMU, and the real trial runner with sleeping helper workloads | Killing the controller mid-trial, every-core zero readback and removal of real systemd trial scopes before the unchanged 60-second restart; controller affinity during the trial, clean-stop behavior and emergency validation/sweep/readback failures | Real SMU access, real stress programs or the production session's tuning decisions |
| `just same` | Both trees' `togi-tool-sim` across every dev and holdout seed of their suites, with journals compared byte for byte (above) | The session, tuner, carry and journal on the paths the suite's scenarios reach, through the simulator's SMU, host, trials and kernel | Any change to trial, detect, smu, hardware or the backends, which the simulator replaces; paths no scenario reaches |
| `just smoke` | One session per scenario of both trees' `togi-tool-sim`, the suite's `smoke` list, compared as `just same` does | The same simulated paths as `just same`, on the sessions with the most crashes and longest histories of each scenario | Anything `just same` does not prove, and the paths the other sessions reach: run `just same` before relying on it |
| `just bench` | Simulated sessions for the chosen split (dev by default) and the model checks; with `--baseline FILE`, a comparison with that baseline by metric thresholds and, when the suite registers one, [the gate](#the-gate) | The same simulated paths as `just same` | Unchanged decisions (one changed decision with unchanged totals can pass), or anything about the packages the simulator replaces |
| `sim-suite-<i>` | Shard `i` of the 220 suite sessions on the release `togi-tool-sim`, as [above](#the-suite-gate); full mode's `check` job requires every shard `nix/sim-suite.json` declares, cached or built | The session, tuner, carry, journal and simulator on the paths the suite reaches: each session concludes, resumes warm state equal to a cold fold, replays through the tuner and passes the invariant audit | Better or equal tuning (bench scoring does that), or anything about the packages the simulator replaces |
| `package` | Builds the release `togi` with the revision `dev` and runs it: `togi --version` and `togi completion` for bash, zsh, fish and nushell, as [below](#the-package) | That the package builds and installs, prints `togi <version.txt>+dev` and generates a completion script that names `togi` for each shell | What any command does, the set of commands, or that a completion script loads in its shell |
| `just hardware` | On the target machine, as root: `cargo test --features hardware --test hardware` on `togi-smu`, `togi-detect`, `togi-trial` and `togi-hardware`, one binary and one test at a time, every binary even after one fails; as any other user it also builds and runs `togi-trial`'s `cached_errors`, which refuses root, and the tests that need root fail. `--no-run` builds them without running | The hardware tests: smu's offset round trip and PM table lanes, detect reading the real kernel log, trial's real systemd scopes (R1 and R2 workloads on core 2 included) and hardware's diagnosis leaving offsets unchanged | R3 to R7 workloads or multi-core loads on real backends, a new offset written to the SMU, the host's full preflight or ranking, or a tuning session: those need `togi doctor` or `togi run` evidence from the target machine. Until the owner runs it at the cutover, smu's, detect's and hardware's tests are only type-checked, by the flake's `clippy` check; `vm-trial-scope` runs trial's in a NixOS VM, all but the workload test, as root and as a delegated user, `cached_errors` included |

## What a run records

`--out FILE` writes one JSON object per run:

- `status`: `concluded` (the session stopped after its clean cycle), `deadend`, `censored` or `error`. `censored` is a session still running when `togi-tool-sim` reached its boot cap (`--max-boots`, default 1000; [simulating](simulating.md)): it did not conclude, and its metrics describe the partial session, so its `sim_hours` and `crashes` are lower bounds on its cost;
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

Bench records compute `worst_r7_hazard_per_h` only for `final_profile`, even when a run is censored. They do not score every journal profile, and no command scores another one: the bench's helper is private to its `metrics` module. To score an intermediate checkpoint, add a scratch unit test to `mod tests` in `crates/togi-tool-bench/src/metrics.rs`. Run it, then delete it; it never lands. The test uses the same machine file and the run's seed, as the bench does:

```rust
#[test]
fn score_checkpoint() {
    let machine = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/bench/machines/MACHINE.json");
    let seed = SEED;
    let checkpoint = r#"PROFILE"#;
    let model = Model::new(&togi_sim::file::load(machine).unwrap(), seed).unwrap();
    let checkpoint: PerCore<TargetOffset> = serde_json::from_str(checkpoint).unwrap();
    let profile: Vec<TargetOffset> = (0..model.cores())
        .map(|index| core(index).and_then(|core| checkpoint.get(core)).unwrap_or(TargetOffset::ZERO))
        .collect();
    let worst = worst_r7_hazard_per_h(&model, &profile)
        .unwrap()
        .expect("a 16-core machine with a shared-voltage R7 model");
    println!("worst_r7_hazard_per_h={worst:.6}");
}
```

```sh
just focus togi-tool-bench --lib --no-capture score_checkpoint
```

Replace `MACHINE.json`, `SEED` and `PROFILE`. `PROFILE` is the checkpoint's profile object as the journal records it, keyed by core ID (`{"0":-20,"1":-18,…}`); a core it leaves out scores at 0. A `profile.change` event's `to` gives the tuning profile after that change; a `trial.intent` event's `profile` gives the entire applied trial profile, which may instead be a parked hunt profile. Choose the one the checkpoint means. Score with the metric and machine from the same pinned revision used for the comparison. This is fitted steady-state risk, without replay draws or an assertion that the checkpoint passed a cycle.

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

`just bench-baseline OUT` writes the same projection to another file and leaves the committed one alone:

```sh
just bench-baseline OUT
```

`OUT` defaults to `tools/bench/baseline.jsonl`. The recipe runs `togi-tool-bench --split all --baseline-out OUT`. The flag writes the same run as `--out` with `model_check` and `wall_s` left out; it may be given together with `--out`, and neither file makes the tree dirty for the `commit` and `dirty` stamps. The projection is `Record::baseline` in the crate, pinned by its tests, not a filter in the recipe. `togi-tool-bench sanity` and `--baseline` read the committed file in this record format.

Compare a candidate with the recorded baseline:

```sh
just bench --split all --baseline tools/bench/baseline.jsonl
```

The active baseline is Ruleset 11, recorded whole by `just bench-baseline` at a clean commit; the Ruleset 10 baseline it replaced is archived with the Ruleset 11 gate. That Ruleset 10 baseline had one partial re-recording, [#563](https://github.com/shgew/togi/issues/563): the two anchor overlays changed from frozen copies to genuine overlays of today's anchor, so only their two 24-seed scenarios (`target-r7-vf-boost`, `target-r7-request-gap`) were re-recorded at `21052983`, and the recording commit was appended to the gate's `baseline.commits` ([#659](https://github.com/shgew/togi/issues/659)).

## Forecasting a real run

Before a hardware run, record what the simulator expects it to do, so that [`just stats` can score it afterwards](reviewing.md#scoring-a-forecast):

```sh
just forecast COPY-OF-STATE-DIR --out tools/bench/forecasts/<session>-<seq>.json
```

The ensemble is the suite's `target` scenario with all its dev and holdout seeds, each run on its fitted member with real-fact replay, so new members join the forecast unchanged. For each run, the tool copies the given directory into the run's own directory as `just copy-state` does (below): `events.jsonl`, `state.json` and every archive entry the journal reads sessions from (archived `archive/<session>.jsonl` journals, the `archive/<session>-carry-pending` and `archive/<session>-compat-pending` markers of an interrupted transition, and `archive/<session>-reset-all` markers). It resumes that copy with `togi-tool-sim` until it concludes, as `togi run --cycles 1` would: under the configuration the copy's latest `config.loaded` recorded, with its backend store paths, durations, evidence and checking cycle, so the passes on record keep counting. A live journal of an older schema contributes only its backend store paths, on top of the default configuration ([simulating](simulating.md)). That recorded snapshot stands in for the configuration file the real machine would load, so a forecast matches the next real run only while that file is unchanged since the recorded `config.loaded`; the given directory is only read. An archive entry of those names that is not a regular file, such as a symlink, fails the forecast with an error naming it.

From a checkout on the target machine, copy those files with `just copy-state` and forecast from the copy (the sources are read-only). The state directory is root's, so the copy runs as root, building under root's home, and the copy is then handed back to you. Take the copy while togi is not running a trial: a journal that ends inside a trial resumes, in every run, as a crash of that trial.

```sh
copy=$(mktemp -d)
trap 'rm -rf "$copy"' EXIT
sudo nix develop --command just copy-state /var/lib/togi "$copy"
sudo chown -R "$(id -u):$(id -g)" "$copy"
just forecast "$copy" --out tools/bench/forecasts/<session>-<seq>.json
```

`just copy-state SRC DST` copies `events.jsonl`, `state.json` when present and every archive entry named after a session (archived journals and the carry, compatibility and `reset --all` markers) into `DST`, creating it; it leaves out `trials/` and everything else. An archive entry so named that is not a regular file, such as a symbolic link, fails the copy naming it before anything is written.

The anchor is the last complete event of the copied live journal: its session, sequence and time, with a SHA-256 of the session's journal lines through it. Only events after the anchor count, and the outcome ends at the run's first conclusion or dead end after the anchor: events after that point do not count. A simulated run stops there; a real run that kept checking past its conclusion, as a tuning boot does, is therefore scored by `just stats` as what a `togi run --cycles 1` run with the same history would have recorded. For each run, the forecast records:

- `status`: `concluded` (the run reached the clean cycle `togi run --cycles 1` stops at), `deadend`, or `censored` (still running at the simulator's boot cap). A run that errors fails the whole forecast;
- `hours`: run time after the anchor through the conclusion, or through the last event of a run that did not conclude, counted as `just stats --forecast` counts a real run's ([Scoring a forecast](reviewing.md#scoring-a-forecast)): invocations and the downtime before one that detects a crash, not clean stops;
- `crashes`: `crash.detected` events;
- `hunts`: hunts that ran a trial; a hunt answered only by carried facts runs none;
- `profile` and `depth`: the newest session's final profile and its sum, as in `just bench` (its checking profile, or its baseline before one exists).

The output names the anchor, the commit, the ruleset and the runs by status, then for each core's offset, the depth, hours, crashes and hunts the median, the 10th to 90th percentile and the minimum to maximum. Percentiles use the nearest rank, so each is a value some run produced; the median of an even count averages the middle two. Hours cover concluded runs only; the other rows include every run, so the crashes and hunts of runs that did not conclude enter as lower bounds. The model checks follow, as in `just bench`. Hours are simulated time, reported as is and not corrected: a simulated crash reboot takes 90 s, while real recovery took a median of 141 s on 2026-10-03, about 37 minutes over that session, so a real run takes longer than its forecast hours. The output prints this bias too.

The output and the record are identical for a fixed copy, commit, suite, machine files and facts extract: they hold no wall times or local paths. `--out FILE` writes the record as JSON. Name it `tools/bench/forecasts/<session>-<seq>.json` after the anchor and commit it in a pull request that merges before the run starts, so the history shows that the forecast predates the run and anyone can score it again with `just stats`. The record holds the anchor's identity and simulated outcomes only, no journal content.

The crate `togi-tool-forecast` defines the record, which `togi-tool-bench forecast` writes ([below](#the-forecast-command)) and `stats --forecast` ([#820](https://github.com/shgew/togi/issues/820)) scores. It holds the anchor; the short commit, a dirty flag and the ruleset; the evidence epoch; `files`, each machine file and facts extract the ensemble read, by path relative to the suite and SHA-256; each run's machine, seed, split and `outcome`, a nested object (status, hours, crashes, hunts, the profile as an object of core to offset, and the depth); and the summary of statuses and ranges, `summary.cores` an object of core to range. It hashes the anchor session's journal lines as the file holds them. The crate refuses a record in another layout, a Go-written one included, and an array or an unknown key where it expects an object. The build reads no Go-written journal, state or archive ([Reset and compatibility](spec/runtime.md#reset-and-compatibility)), and likewise no forecast the Go tool wrote.

`--split`, `--baseline` and `--same` are usage errors with `--forecast`. `--keep DIR` retains every run directory under a new directory in DIR. Without it, each successful run is deleted as it finishes; a forecast whose runs failed keeps the failed runs and prints where, and a forecast that fails after every run succeeded, such as on writing `--out`, removes its run directory and prints none.

## Counterfactual pass and cost of the R7 requirements

`just pass-cost` computes, without simulating, what the checking cycle's R7 part requirements cost at a fixed profile when every trial fails with a known probability ([#490](https://github.com/shgew/togi/issues/490)). It is readiness tooling for #490's observation-only study: it reads no journal, writes nothing, and no number it prints is tuner evidence or feeds a tuner rule. Running it is not running that study.

```sh
just pass-cost [--hazard PER_HOUR,…] [--ccds 2] [--config FILE] [--launches 40] [--hours 4] [--extra-short N,…]
just pass-cost --machine FILE --profile OFFSETS [--seed 1] [--workload ID] [--cores CORES] [--config FILE] [--launches 40] [--hours 4] [--extra-short N,…]
```

Each part's requirement comes from the tuner (`togi_tuner::r7_part_passes`), under `--config` or the built-in defaults: three passes at `short_trial_s` and one long pass whose duration depends on the part and the CCD count ([Checking](spec/tuner.md#checking)), so the defaults on two CCDs ask 3×120 s + 1×300 s of a CCD or partial part and 3×120 s + 1×600 s of the all-core part. The failure probability of each trial comes from one of two sources:

- **Constant hazards** (the default): `--hazard` lists failures per hour, by default #490's grid 0, 0.1, 0.5, 1, 2, 4, 8 and 16. A trial of `d` seconds fails with probability `1 - exp(-h·d/3600)`, with no onset boost. `--ccds` sets the long durations; a partial's requirement is its CCD part's.
- **A simulator machine**: `--machine` and `--profile` (one offset per core) evaluate the machine's model at that fixed profile with `failure_probability`, every source counted, background, onset boost and delays included. For each R7 workload (or `--workload`), the step's parts are each CCD's full part, its request-ordered partials of at least two cores when the machine has a shared rail, and the all-core part on several CCDs, as `worst_r7_hazard_per_h` derives them ([What a run records](#what-a-run-records)); `--cores` evaluates only the part that loads those cores. `--seed` draws the hidden limits of a machine file that sets none. A step row chains its parts.

For each part, the output prints the passes, each trial's failure probability and:

- `first`: the probability that every required trial passes on its first launch, the exact product of `1 - p` over the trials. This is the chance that a part at this profile qualifies, including by luck at a high hazard.
- `launches` and `hours`: the expected launches and time to meet the requirement when it is retried at the same profile until met. Each requirement waits for its passes in a row, `Σ_{i=1..n} (1-p)^-i` launches, since a failure invalidates the passes of its own class and leaves those of the part's other classes ([Evidence](spec/tuner.md#evidence)).
- `done≤L` and `done≤H`: the probability of meeting the requirement within `--launches` launches and within `--hours` hours, by dynamic programming over the passes earned and the launches or seconds elapsed; the censored share is `1 - done`. `spent_n` and `spent_h` are the expected launches and hours spent before the requirement is met or the horizon ends.
- With `--extra-short N`, a row per part with N more short passes, and `+launches` and `+hours`, its expected extra cost over the current requirement.

Only `first` on a current-requirement row describes the current rules; on a `+N short` row it is the first-attempt probability of a requirement the tuner never asks for. Everything else is **counterfactual**, and the output says so: the tuner answers a multi-core R7 failure by backing off ([R7 voltage-targeted backoff](spec/tuner.md#r7-voltage-targeted-backoff)), not by retrying at the same profile, and it never asks for extra short passes. Trials are independent at a fixed profile; a failed trial is charged its full intended duration, so hours are an upper bound; a step counts one R7 occurrence, not the counts a later occurrence of the same workload adds. The probability that a part passes on its first attempt is not the probability that a retried part eventually earns its passes, and the two columns stay apart.

The counterfactual short-pass count caps at `u32::MAX`, the requirement field's ceiling; adding passes never wraps that count.

A trial of 120 s at 2 failures per hour fails with probability 0.0645 and one of 300 s with 0.1535, so a CCD part qualifies on its first attempt with probability 0.6930:

```text
hazard 2.00/h
  part                                 passes         fail_p           first  launches    hours   done≤40 done≤4.00h  spent_n spent_h +launches  +hours
  ccd                                  3x120s+1x300s  0.0645/0.1535   0.6930       4.6     0.21    1.0000     1.0000      4.6    0.21
  ccd +2 short                         5x120s+1x300s  0.0645/0.1535   0.6065       7.3     0.30    1.0000     1.0000      7.3    0.30       2.7    0.09
```

## Counterfactual re-derivation accounting

`just stats --counterfactual` accounts a journal's R7 chain re-derivations ([ADR 0055](adr/0055-rederive-r7-chains-after-a-backoff.md)) under the three candidate rules of [#896](https://github.com/shgew/togi/issues/896)'s brief ([`tools/bench/research/896.md`](../tools/bench/research/896.md)). It is readiness tooling for that campaign: it reads one journal, never changes the tuner, and no number it prints is tuner evidence. Removing a derivation's trials from a journal is not what a tuner with that rule would have done, so every figure is an upper bound and the output says so.

```sh
just stats --state-dir <copied-state-dir> --counterfactual [--witnesses records.jsonl]
```

It folds the journal into the tuner event by event and attributes each `checking.chain` event and its trials as [`--chains`](reviewing.md#the-stats-tool-togi-tool-stats) does. Each candidate decides every derivation before the tuner folds it:

- **coalesce** skips a re-derivation that a later derivation of the same cycle, step and CCD replaces at the same or an earlier part, as if repeated derivations were merged before the chain returned to the step.
- **defer** skips a re-derivation of a step before the cycle's last R7 step, made before that step began, as if earlier steps' re-tests waited for it.
- **tie** keeps a position's earlier set instead of a changed one when every core the earlier set idles requests within one count (`VOLTS_PER_COUNT`) of the CCD's highest request in the order the tuner derived the changed set from (`State::chain_order`); when that order is not the recorded one the case is unresolved. Later positions are re-derived from the kept set with the tuner's order and must match the recorded set to stay resolved.

First derivations are never skipped. A skipped derivation's seconds count only in a cycle that ends passed with every final chain class covered; the rest stay unresolved:

- A derivation whose `source_seqs`, or whose re-derived order's sources, cite a skipped trial, and every position after one the candidate could not re-derive, are unresolved.
- A skipped failing trial is reported as **delayed** when the step's final chain still holds its set (coalesce and defer) and as **lost** otherwise; from it on, every derivation and the cycles it and later ones end are unresolved, since the tuner's backoff would not have happened.
- A tuner decision, hunt, combination, deepening round or core phase whose `cause` cites a skipped trial, such as a backoff whose target rests on that trial's passes or a located hunt those passes justify, ends the candidate's resolved accounting the same way: every later derivation and every cycle ending after it are unresolved, since its offsets could differ.
- At each passed cycle end the coverage is counted by the tuner's evidence ledger (`State::evidence`) for every partial R7 part of `State::cycle_plan`, as the candidate holds the chain: each class's short and long requirements, at the cycle's ending checking profile and within the cycle's window. A part's quota is the tuner's base quota times one more than the earlier parts of its workload that hold the candidate's set there, as the tuner counts its own sets, and a class needs the largest quota among its parts, since a later part's quota already includes the earlier ones'. A pass the candidate skipped, or one of a derivation it leaves unresolved, is no witness: needed passes fall back to those passes, which count as **required** seconds to run again, then to **unobserved** passes the journal does not have, also required at their duration.

`avoidable_s` is the skipped seconds less the required seconds, and may be negative. Every chain trial second is kept, skipped or unresolved, so the three add up to the chain trial seconds; reruns belong to no cycle and are not counted. The **Candidates** table prints these totals per candidate with the avoidable hours and their fraction of the time from the session's first event to its first clean cycle's end (or its last event). **Skipped failures** lists each lost or delayed failure.

`--witnesses FILE` writes the records as JSON Lines, tagged by `record`: `derivation` for each derivation a candidate does not keep (`verdict` `skipped` or `unresolved`, with `reason` `after_failure`, `after_decision`, `source`, `diverged` or `order`, its trials and seconds); `failure` for each skipped failure; `cycle` for each cycle that held a derivation, with `status` (`covered`, `diverged`, `unidentified` when the final chain or its evidence cannot be told, `abandoned`, `open`), the ending `profile` and per class `needed` passes, the `witnesses`' and `rerun` passes' `trial.end` sequences, `unobserved` and, for a tie-kept set, `instead_of` the recorded one; and one `summary` per candidate.

`just counterfactuals RUNS OUT` runs `--counterfactual --witnesses` and `--chains` over every development session of the one bench run a `--keep RUNS` directory holds, writing four files per session to `OUT`; it refuses a directory holding several runs, and names each session it cannot read, which gets no files, exiting 1 once it has accounted the rest. #896's brief runs it over its frozen corpus, `tools/bench/research/896/suite.json`.

## Research campaigns

A research campaign is a finite run of experiments by an agent following one issue's brief. Only the owner starts one, naming the issue, the limits and the output directory; a brief, a readiness change or a scheduled issue is not authorization. This section owns the rules every campaign shares; a mode's program and an issue's brief add only their own.

### Modes

| Mode | Rules | May change | Ends in |
|---|---|---|---|
| Model development | [`tools/fit/program.md`](../tools/fit/program.md) | the failure model and the fitter | a report; a kept model becomes a model-only pull request |
| Production policy experiment | [`tools/bench/program.md`](../tools/bench/program.md) | tuner decisions in `crates/togi-tuner` | a report; kept changes become sub-issues of the open `Ruleset N` |
| Fixed-policy observation | the issue's brief | analysis tools and research-only fixtures; the tuner, configuration and simulator stay unchanged | a report |
| Counterfactual accounting | the issue's brief | the same as observation; alternatives are computed offline, never executed | a report |

Observation and accounting measure the unchanged tuner. Their conclusions about another policy are labelled counterfactual: removing events from an observed journal is not what a different tuner would have done. #490 and #896 start only in these two modes. A simulated gain never lifts their evidence freeze or the [evidence volume](../tools/bench/program.md#hard-rules) rule; a production change for either needs a validated #480 forward score and an owner-approved design under #979.

| Issue | Mode | Brief |
|---|---|---|
| [#427](https://github.com/shgew/togi/issues/427) | model development | [`tools/fit/research/427.md`](../tools/fit/research/427.md) |
| [#480](https://github.com/shgew/togi/issues/480) | model development, diagnostic only | [`tools/fit/research/480.md`](../tools/fit/research/480.md) |
| [#490](https://github.com/shgew/togi/issues/490) | fixed-policy observation | [`tools/bench/research/490.md`](../tools/bench/research/490.md) |
| [#896](https://github.com/shgew/togi/issues/896) | counterfactual accounting | [`tools/bench/research/896.md`](../tools/bench/research/896.md) |
| [#503](https://github.com/shgew/togi/issues/503) | fixed-policy observation, feasibility only | [`tools/bench/research/503.md`](../tools/bench/research/503.md) |

An issue without a brief has no campaign; #428's brief waits for #427's parameterization. Each brief names its mode, its finite candidate queue, permitted and forbidden data, writable paths, metrics, keep and stop rules, result format and prerequisites. A prerequisite that has not landed stops the campaign before its first candidate. Every campaign runs through the `just research` dispatcher, which validates its manifest and writes its record ([below](#the-dispatcher)).

### Limits

By default a campaign runs at most 8 hours of wall time and 12 substantive candidates, evaluates with 4 workers (`--jobs 4`), and holds the host-shared benchmark lock ([AGENTS.md](../AGENTS.md#commands)) for each benchmark it runs. It stops at the first exhausted limit. A substantive candidate is one entry of the brief's queue evaluated against the reference; reference runs and a confirmation belong to the candidate they serve. The owner may override any limit at launch. A monetary or API spending cap is a launch parameter of the agent runner, never chosen by a brief; without one, a paid agent does not continue unattended.

The bench's per-simulator `--timeout` and `--max-boots` bound one simulated session. They are not campaign limits, and a campaign keeps them.

### Prohibitions

In every mode a campaign never:

- runs `togi run`, `togi reset` or `just hardware`, or touches hardware, the SMU, the watchdog, the boot loader or a live state directory;
- refreshes evidence (`just facts`), forecasts (`just forecast`), regenerates fitted machines, the anchor or overlays into `tools/bench/machines/`, or re-records a baseline (`just bench-baseline`);
- acquires target evidence, deploys or merges;
- writes outside its brief's writable paths and the campaign's output directory.

Launch it unprivileged, in its own worktree, with no access to live state or hardware and no privileged credentials. The manifest and diff checks catch mistakes; they are not a security sandbox.

### The record

A campaign writes its record to the output directory the owner names, outside the checkout, so nothing it commits can include it:

- `manifest.json`: a byte copy of the launch manifest: issue, mode and hypothesis queue; checkout and reference revisions; the approved session allowlist and canonical input hashes; the suite and split, which fix scenarios and seeds; frozen-file hashes, which always include the brief and the build environment (`Cargo.lock`, `flake.lock`, `rust-toolchain.toml`); writable paths; limits; output directory; and the confirmation allowance. A missing or changed manifest, or a frozen input whose hash changed, stops the campaign; nothing refreshes it silently.
- `experiments.jsonl`: append-only, never edited. Each evaluation run has a start line, written before it runs, and an end line. A confirmation run has its own lines, which name the candidate they serve. A start line without an end line is an incomplete run. A start line holds the candidate, its role, the command, `HEAD`, the hash of the candidate's changes, the reference revision and every pinned hash. An outcome line holds the typed outcome, its reason, full-precision metrics and the runs it rests on. What killed or failed writes left, the bytes after the last newline and any cut-off lines before them, is never a record, even a complete record that lost only its newline: the next append ends it with a newline and keeps it, hex-encoded, in a `ledger_torn` line. A complete line that does not decode stays an error. A `confirmation_failed` outcome or a timed-out run stops the campaign even if a torn write lost the stop line written with it.
- the raw stdout and stderr of every evaluation command;
- `report.md`: the final report.

Rules for the record:

- Decide each outcome from the typed metrics and the brief's keep rule, never from exit status: the bench exits 0 on REJECT and NEUTRAL, and model flags change neither verdict nor exit ([Comparing two versions](#comparing-two-versions)).
- Never select a winner from an incomplete or unpaired comparison. Record every candidate, rejected, negative and invalid ones included.
- On rejection, restore only the experiment's own changes and keep unrelated work.
- A confirmation is spent when it starts: its start line binds the candidate, reference and data hashes. An interrupted confirmation stays spent, and a resumed campaign never repeats it.
- A resumed campaign appends to the same ledger; its interrupted experiments stay incomplete.

The campaign stops with one reason: its queue is exhausted, a limit is exhausted (wall time or candidates), a confirmation failed, evidence or a frozen input changed, a forbidden diff appeared, a comparison was unpaired, a prerequisite is missing, or it was interrupted.

### The dispatcher

`just research ISSUE ACTION` runs one step of a campaign; it is `togi-tool-bench research`. It chooses no experiment and runs no agent: the researcher calls it for each step, and it refuses whatever the brief and manifest do not allow. Every action that writes holds the run directory's lock without waiting, so a second dispatcher on the same campaign is refused.

| Action | What it does |
|---|---|
| `check --manifest FILE` | Validates the manifest against the tree and creates nothing. |
| `start --manifest FILE` | Validates the manifest, requires `HEAD` at its checkout with no changes, a reference that differs from it only at writable paths and a new output directory outside the checkout, copies the manifest and writes the ledger's first line. |
| `exec --run DIR --candidate ID --role ROLE -- RECIPE ARGS` | Runs one evaluator through `just`, as described below, keeping its stdout and stderr under `raw/`. |
| `outcome --run DIR --candidate ID --outcome OUTCOME --reason TEXT --metrics FILE` | Records the researcher's outcome (`kept`, `rejected`, `invalid`, `confirmation-passed` or `confirmation-failed`) with the metrics it read, as a JSON object of numbers. |
| `stop --run DIR --reason REASON` | Stops the campaign with one of the stop reasons above. |
| `resume --run DIR` | Records the runs a killed dispatcher left unfinished as incomplete, then lets the campaign continue. |
| `report --run DIR --notes FILE` | Writes `report.md` from the ledger and the researcher's notes on the remaining uncertainty. |

A manifest is JSON. Hashes are SHA-256 in lowercase hex; a gzip input is pinned by its decompressed bytes. `inputs` is for model development and `suite` (`path`, `sha256`, `split`) for observation and accounting. `limits` defaults to the values above. The owner writes, for example:

```json
{
  "issue": 427,
  "mode": "model-development",
  "hypotheses": [{"id": "shared-core", "statement": "a shared per-core term improves the forward score"}],
  "checkout": "<40-hex commit>",
  "reference": "<40-hex commit>",
  "inputs": [{"path": "<approved artifact>", "sha256": "4b47590e…",
              "development": ["20260924T204352Z", "…", "20261002T235548Z"], "confirmation": ["20261004T000701Z"]}],
  "suite": null,
  "frozen": [{"path": "Cargo.lock", "sha256": "…"}, {"path": "flake.lock", "sha256": "…"},
             {"path": "rust-toolchain.toml", "sha256": "…"}, {"path": "tools/fit/research/427.md", "sha256": "…"}],
  "writable": ["crates/togi-sim/src"],
  "limits": {"wall_minutes": 480, "candidates": 12, "jobs": 4},
  "output": "/absolute/path/outside/the/checkout/run-1",
  "confirmations": 1
}
```

The dispatcher refuses a manifest whose issue has no brief or names another mode, whose sessions are not that issue's approved ones, which leaves the environment or the brief unpinned, or whose writable paths overlap evidence, fitted machines, baselines, the tuner, the run loop, the hardware path, the dispatcher, the programs and briefs, the build environment or a pinned file. Observation and accounting may not write the simulator or the fitter either.

Before `exec` and `outcome` record anything, the dispatcher checks that the campaign has not stopped, that no run is left unfinished, that wall time remains, that every pinned file still has its hash and that each facts input still holds exactly its named sessions. It also checks that `HEAD` descends from the checkout and that every changed path is a writable one. A failed check after the start stops the campaign with its reason. A refused command records nothing.

If the current wall time precedes the campaign's recorded start, the dispatcher refuses the action with an elapsed-time error and records nothing. It does not treat an inverted interval as zero or restore the full wall-time budget.

`exec` allows only its mode's evaluators. Model development's `forward` evaluator invokes `just fit --forward-only` (`--facts`, `--seal`, `--jobs`, `--records`); `fit` finalizes a confirmed candidate (`--facts`, `--out`, `--jobs`, `--seed`, `--bootstrap`), alongside the F5 `bench` on the committed suite with `--split all --baseline tools/bench/baseline.jsonl`. `--facts` must name a manifest input, never the committed extract. A development run seals exactly the input's confirmation sessions, and a confirmation scores them unsealed. Observation and accounting run only `bench`, with the manifest's `--suite` and `--split`, and keep `--timeout` and `--max-boots`. Output flags must name paths inside the run directory, never its own record (`manifest.json`, `experiments.jsonl`, `lock`, `report.md`, `raw`), and `--jobs` defaults to the worker limit and may not exceed it. Each run waits for the host-shared lock, then makes its checks and gets the wall time left. When the time runs out, or the dispatcher is told to stop, it terminates the evaluator's process group and records the run as timed out or interrupted. A timed-out run also stops the campaign.

An observation or accounting campaign analyses what its `bench` run kept with tools that simulate nothing, such as `just stats --backoffs` over the kept journals and `just pass-cost`. They are not evaluators and run outside `exec`: they read the run directory and the frozen fixtures, write only under the run directory, and the report names each command. [#490's brief](../tools/bench/research/490.md#procedure) is the worked example.

The roles enforce the comparison's order:

- A `reference` run needs the tree at the reference revision.
- A `candidate` run past the candidate limit stops the campaign.
- In model development, `kept` needs a complete candidate run paired with a complete reference run of the same command and data, where each run's `--records` path may differ; otherwise the campaign stops as unpaired. Observation and accounting need a complete candidate run.
- A `confirmation` needs a kept candidate whose changes are unchanged since its run, and a confirmation the manifest allows. It is spent when its start line is written. Its `confirmation-reference` run follows it, and its outcome needs both to be complete.
- `confirmation-failed` stops the campaign. A `finalization` run needs a passed confirmation at the same changes.

### End state

A campaign completes its approved experiments, records every outcome, writes the report, may open a reviewable result pull request when its brief allows one, and stops. The report reconciles the ledger, the exact comparisons, the limits used, the confirmation status and the remaining uncertainty; it names the run directory only as `$RUN` and lists any outcome that rests on an incomplete run. A negative, no-change or interrupted campaign is a complete deliverable, never a reason to relax a gate. Published summaries and artifacts follow [Writing](../AGENTS.md#writing): no private state paths, raw host journals or identifying data.

## The bench (`togi-tool-bench`)

`just bench` runs `togi-tool-bench`: the runs, metrics, summary, comparison and cache of the bench ([#821](https://github.com/shgew/togi/issues/821)). `just same`, `just smoke` and `just forecast` run its `same` and `forecast` commands ([#823](https://github.com/shgew/togi/issues/823)), described [below](#the-same-command). `just pass-cost` runs its `pass-cost` command, [above](#counterfactual-pass-and-cost-of-the-r7-requirements). `just research` runs its `research` command, the [campaign dispatcher](#the-dispatcher). The gate report is [below](#judging-the-gate) ([#822](https://github.com/shgew/togi/issues/822)).

```sh
just bench --split dev --baseline BEFORE.jsonl --out AFTER.jsonl
just bench sanity --records AFTER.jsonl
```

The suite is `tools/bench/suite.json`, read by strict rules (unknown keys refused, a machine file per scenario, a replay oracle for `replay: true`); every scenario's machines and replay oracle are loaded and checked whatever `--split` selects. A run builds `togi-tool-sim` from this tree with `cargo build --release --locked`, copies the binary into a private directory, and starts that copy as a subprocess for every run with a temporary `--state-dir` whose journal the bench reads after it exits ([ADR 0060](adr/0060-bench-children-keep-temporary-journals.md)), in a process group of its own, with the flags `just sim` takes; `--sim PATH` copies and runs a given binary instead. `--jobs` (default: the CPU count) bounds the simulators at once, `--timeout SECONDS` is the hang safety limit of one run (an overrun is a harness error, not a run outcome), and `--max-boots` (default 1000) is the boot cap that makes a run censored. A harness error stops the simulators still running and is what the bench reports. A session a signal killed or stopped is an `error` record and is not cached. `--keep DIR` retains every run directory and skips the cache. The records come out in suite order whatever `--jobs` is, so they do not depend on scheduling. The bench records the tree's commit and dirty flag, so it runs inside a Git work tree. Ctrl-C stops the simulators that are running, discards kept run directories and exits non-zero.

After a run's record is read and before its state directory is removed, the bench audits every journal of the directory, archives included, with [the audit](#the-audit-togi-tool-audit)'s library ([#546](https://github.com/shgew/togi/issues/546)). A `concluded` or `deadend` run is audited as finished, so its live journal must end in its conclusion and its `state.json` must equal a fresh fold; a `censored` or `error` run is audited as an unfinished prefix. A journal the lenient read left anything out of is a warning. The report prints `audit: audited=N violations=M` after the model checks, then each violation as `audit: RUN [JOURNAL] [seq N] CHECK: reason`; a run with a violation is not cached, and the bench exits 1 with `the journal audit found M violations` after writing its records and report, whose gate verdict, with a baseline, is still printed: the audit's exit comes before the gate's `gate failed`. A cached record was audited when it was made. The `same` and `forecast` commands audit nothing: `same` compares journals byte for byte, and `forecast` continues a copy of a real state directory, which `just audit` covers.

The record is JSON Lines: every field is required except `machine`, `model_check`, `wall_s` and `worst_r7_hazard_per_h`, which are left out when absent (a baseline file written by `--baseline-out` leaves out `model_check` and `wall_s` on every line; comparison and gates read neither, so `--baseline` prints the same against it as against the full `--out` records) (`first_passed_cycle_h` must be present and may be null), an unknown key is refused, `final_profile` is an object of core ID to offset (`{"0":-23,"1":-15,…}`: the last `profile.change`, or the session's baseline before one), `hazard_per_h` an object by regime, `model_check` counts `eligible_groups`, `flagged_groups` and `idle_failures` (the flagged groups themselves are in the printed report), and `evidence` and `ruleset` are the build's `togi_tuner::EVIDENCE_EPOCH` and `RULESET`. A run's real answers come from the same extraction as the facts extract and count only decisive trials whose `trial.start` was journaled. The comparison, summary and verdict rules (V1–V4, the equal-scenario-weight bootstrap seeded with 1, 10000 resamples) are those of [Comparing two versions](#comparing-two-versions), except that the warning about unmatched runs counts the candidate runs without a baseline and the baseline runs in the candidate's splits without a candidate directly, so it never prints a negative count. A record file with a run twice, or of the earlier Go format, is refused: `--baseline` takes records of this format only.

The cache holds finished records under `$XDG_CACHE_HOME/togi/bench` (or `~/.cache/togi/bench` on Linux and `~/Library/Caches/togi/bench` on macOS; `--cache DIR` and `--no-cache` override), a directory per tree key and the eight most recently used kept. The tree key binds the SHA-256 of the simulator copy that runs, the bench's own executable's SHA-256 and `--max-boots`; an entry also binds the suite file's bytes, the run's scenario, split and seed, the replay flag, and the bytes of the machine file, every file it extends and its facts extract. Any change misses. A failed cache write is a warning; the record is kept. A cache hit restamps the commit and dirty flag, so a cached record is the one a fresh run would write.

`togi-tool-bench sanity --records FILE` compares a run of the whole suite with the scores in the recorded `tools/bench/baseline.jsonl`. A change to the tuner or simulator can move any seed's outcome, so what must agree is each metric's mean over a scenario's seeds. A line is flagged when the means differ by more than the metric's absolute floor and its relative band (5% for hours and trials, 3% for depth) and by more than 3 standard errors (Welch within a scenario; for the total, stratified by scenario), so only a difference the seeds' spread does not explain counts. Any drop in the share of concluded runs is flagged by its size alone, and so is a metric that one side measured and the other did not, or measured on a different number of runs; the table prints both run counts. The bands are in `crates/togi-tool-bench/src/sanity.rs`. It exits 1 when any line is flagged: a flag is a question to answer, not a gate. With 8 runs a side, 3 standard errors is a two-sided p between about 0.01 and 0.02 under Student's t, and the table has about 150 lines, so a flag in a small scenario is expected now and then by chance; answer it with fresh seeds of that scenario on the candidate and on the commit that recorded the baseline.

### Judging the gate

`just bench --split all --baseline BASELINE.jsonl` judges the `gate` object of the suite, when it registers one, after the generic comparison, prints each criterion and an overall `gate ID: PASS` or `FAIL`, and exits 1 (with `gate failed` on stderr) unless it passes. The criteria and the refusals are those of [the gate](#the-gate): conclusion, the median and 90th percentile of the final-profile `worst_r7_hazard_per_h` by the interval of the change, the time ratio, and the pooled median. A gate without `--baseline` is read and checked, and nothing is judged; a suite without a gate prints none. The baseline is a file of records as `--out` of an earlier run wrote it; a file of Go records is refused.

The gate object is typed, strict and checked before any session runs: an unknown key is refused; `split` is `dev`, `holdout` or `all`; `bootstrap_seed` is one integer; `resamples` is 1 to 10,000,000; a scenario is not listed twice, nor both gated and concluded (a gated scenario's conclusion is already a criterion); a quantile label keeps its decimals (`p99.5`). The bootstrap resamples seed pairs with a [`StdRng`](https://docs.rs/rand/0.10.3/rand/rngs/struct.StdRng.html) seeded with `bootstrap_seed`, one generator per quantile criterion, drawn on one thread in a fixed order, so an interval is the same on every run and on every machine with any number of threads or cores. A time criterion with no pair that both sides concluded fails and prints `ratio=none timed=0`.

**Archived Go gates are not re-scored.** The Ruleset 11 archive's bootstrap draws came from Go's PCG, which the bench does not port, and its records are Go's, so the bench cannot reproduce its intervals. The archive stays as the record of what was scored: its four files are pinned by hash (`the_archived_go_gate_is_kept_unchanged_and_never_re_scored` in the bench crate), and no `just bench` mode judges existing records. The bench refuses the archive before any session runs: a gate whose `bootstrap_seed` is a pair, Go's PCG seed, stops the run with `archived Go gates are kept as records and are not re-scored`, and the record reader refuses the archive's `baseline.jsonl` and `candidate.jsonl`. The next ruleset's gate is registered in the current format and scored by the bench.

### The `same` command

`just same [base]` and `just smoke [base]` export `base` (default `origin/main`) with `git archive`, as `just same` does, and run `togi-tool-bench same BASE`. The command is `togi-tool-bench same BASE [--head DIR] [--suite FILE] [--smoke] [--keep-going] [--no-cache] [--keep DIR] [--jobs N] [--max-boots N] [--timeout SECONDS] [--cache DIR] [--base-sim FILE] [--head-sim FILE]`: the head is the current directory unless `--head` names another. Each tree's own suite and machine files are read (a relative `--suite` resolves inside each tree), sessions pair by scenario, split and seed, and with `--smoke` only the sessions the head's suite lists under `smoke` are compared; a session the other tree lacks is a difference. The bench judges nothing else: no metrics, model checks or summary. `--split`, `--baseline` and `--out` do not exist for it, so they are usage errors (exit 2). It exits 0 when no session differs, 1 on a difference or any failure.

Each tree's `togi-tool-sim` is built with `cargo build --release --locked`, unless `--base-sim` or `--head-sim` gives a binary. The head builds in its own `target` directory. The base builds in `same-target` under the cache directory, which every comparison reuses so that its dependencies stay built; because cargo takes a source file older than its last build for unchanged, and the base is an export whose files keep their commit's time, the bench marks every file of the base modified now before it builds, so only the base's own crates build again. A target directory is never shared between the two trees, nor between comparisons at once: a comparison holds a lock on `same-target.lock` in the cache directory from marking the base's files to copying its binary out of `same-target`, so comparisons that share a cache directory take turns, and one that waits says so on stderr and stops waiting at an interrupt. A relative `--cache` resolves against the bench's working directory, not the base tree cargo builds in. A session is reduced to the SHA-256 of its journals and its simulator's exit status. The journals are the live `events.jsonl` and every `archive/*.jsonl`, each digested with its name and size; `trials/` samples and the archive's markers are not journals. Every line is compared as the file holds it, except the two events that carry the build stamp, `session.start` and `config.loaded`: their `version` and `rev` are removed, the build's description is taken out of the message that names it, and the rest of the event is compared as the JSON value it holds, so a change of build alone is not a difference and a change of any decision, `schema`, `ruleset`, `fixes` or `evidence` is. A session with no journal, or only empty ones, is an error, never a match.

A difference prints the session, the first journal that differs and its first differing line from each side (`<missing event>` past the end of one side, `<missing file>` for a journal one side lacks, `<exit code>` when only the exit statuses differ), then `same: N of M sessions differ`. The bench stops at the first difference it finds and says so unless `--keep-going` asks for every differing session. The run directories of a session that matches are deleted as soon as it is compared; a differing session keeps its `base` and `head` directories under a new directory in the temporary directory (`TMPDIR`; or in `--keep DIR`, which keeps every run and implies `--no-cache`), and the bench prints where. An interrupt (SIGINT, SIGTERM, SIGHUP) stops the simulators and removes the runs, those of `--keep` included, and exits 1 as an interrupted comparison, never as equality, also when it came during a cargo build. A run that fails, or the first difference when stopping at it, stops the simulators still running, and the error reported is the failure, never the interrupted runs it caused.

Each side's digest is cached under `same/<key>` in the cache directory (`--cache DIR`, default `$XDG_CACHE_HOME/togi/bench` or `~/.cache/togi/bench`), the eight most recently used kept. The key binds the simulator binary's SHA-256 and `--max-boots`; an entry also binds the session's identity, its replay flag and the bytes of its machine files and facts extract, by name relative to the suite, but not the suite file, so adding a scenario leaves the others cached. A base that does not change the simulator is a cache hit across commits, and when both trees build byte-identical simulators and read the same inputs the bench says so and runs nothing. A side with a cached record is not run again, but a difference reruns a cached side to have its journal to show. Sessions run longest first by their recorded times. The bench reports the summed time of the compared simulators and the sessions where the head took more than twice its base's as information that never decides the verdict; this is the wall time of the simulator process, which is noisy when sessions outnumber cores.

### The `forecast` command

`just forecast STATE_DIR [--out FILE] [--suite FILE] [--keep DIR] [--jobs N] [--max-boots N] [--timeout SECONDS] [--sim FILE]` runs `togi-tool-bench forecast`, which does what [the forecast above](#forecasting-a-real-run) describes with the simulator: it copies the directory into each run's own directory as `just copy-state` does (a link in it, or an archive entry named after a session that is not a regular file, fails the run), resumes the copy on `togi-tool-sim` for every dev and holdout seed of the suite's `target` scenario, and counts only the events after the anchor. The copy is read, never written. An exit 1 is a dead end only when the run reached one after the anchor: a `deadend` event after it, or the simulator's own `sim: dead end` log line; a dead end the copy already held is not the run's. An exit that is neither a conclusion, a dead end nor the boot cap, or a simulator that records no event after the anchor, fails the forecast.

The record is `togi-tool-forecast`'s `Record` with the commit and the dirty flag the command reads from git in its working directory, the ruleset and evidence epoch of this build, `files` (each machine file and facts extract the ensemble reads, by path relative to the suite, with its SHA-256, in the order the machines list them: a machine file before the files it extends) and the runs. The printed report names the anchor, the commit (with `(dirty)` when the tree has changes other than `--out`), the ruleset and the runs by status, then a table of each core's final offset, the depth, hours, crashes and hunts as median, 10th to 90th percentile and range, and the target machines' model checks. It holds no wall time and no local path: a machine file is named by its path relative to the suite's directory, in the record and in the model checks, however the suite names it, and a file outside that directory is refused. A failed run stops the simulators still running, and the forecast names the runs that failed, not those it stopped. A forecast whose runs failed keeps the failed runs' directories and prints where; one that fails later, on writing `--out`, or is interrupted keeps nothing; `--keep DIR` keeps every run, an interrupted one's directory included. There is no flag that conflicts with `forecast`: `--split`, `--baseline` and `--same` do not exist for it.

## Checking a machine against real evidence

A machine file may declare `"facts": "../facts/target.jsonl.gz"`, resolved relative to the machine JSON file. Every `target-fit-*.json` declares the committed target-machine extract. Files without `facts` are not checked.

Regenerate the extract from a temporary copy of the state directory, never the live state: `togi-tool-facts` takes the directory's writer lock. Copy it as for [a forecast](#forecasting-a-real-run), while togi is not running a trial:

```sh
copy=$(mktemp -d)
trap 'rm -rf "$copy"' EXIT
sudo nix develop --command just copy-state /var/lib/togi "$copy"
sudo chown -R "$(id -u):$(id -g)" "$copy"
just facts "$copy"
```

`togi-tool-facts` reads every archived and live session through `togi-tool-trialfacts` and the `togi-facts` extraction, including sessions before `reset --all` and older builds. Its gzip JSON Lines output is byte-stable for fixed inputs. Generation rejects sources with no facts and writes to a temporary file beside the destination; the committed extract is replaced only after extraction and file close succeed, so an unreadable or empty source leaves it unchanged. Records retain session, build, ruleset, BIOS context, sequence, trial ID, kind, class, condition, phase, record-only status, full profile, outcome, signal, measured duration and, when present, `voltage_requests_v`, `top_requesters`, `ccd_mhz` and `stalled_core`. These are machine measurements, not personal data. For older trial ends without request fields, the generator summarizes surviving trial samples using the source session's recorded topology and loaded cores, with the same warmup and minimum-sample rules as live telemetry. They omit journal timestamps, paths, hostnames and boot IDs. `togi-tool-trialfacts` is the shared reader for evaluation tools.
The extract also retains an optional attributed `core` on failures, including core 0, so a named computation error constrains the core it actually names rather than the first loaded core. Older records omit it.

The native reader does not import Go-era journal headers. The committed 2026-10-17 refresh used the unmodified Go extractor at `ec7452cbd37fc826fab96fe4d0b877e040668c94`, with persisted per-trial samples to recover request and clock summaries. Its provenance and once-only score are [recorded below](#the-2026-10-17-frozen-score); this is not a native journal compatibility promise.

The model check groups decisive trials by BIOS context, trial class (regime, workload, sorted loaded cores and intended duration), and the shallowest loaded-core offset in the applied profile. Any loaded core at 0 puts the trial at depth 0. If the machine declares a BIOS context, only matching facts are checked; otherwise contexts are checked separately.

This check uses decisive pass/failure trials only; interrupted and inconclusive trials are omitted. A facts-declaring machine with positive `thermal_trip` or `power_loss` weights in `model.reset` is rejected: those resets can turn simulated crashes into inconclusive outcomes, while the simulator's failure probability includes the full failure hazard. Comparing that probability to decisive-only trials would use incompatible denominators.

For each group with at least 10 trials, the simulator computes each trial's exact failure probability over its intended duration, including onset boosts, unloaded-core hazards and delayed joints. The check averages these probabilities and compares observed failures against quantiles 0.005 and 0.995 of Binomial(n, mean_p). A count outside that inclusive interval flags the machine. Idle failures are retained as their own fact class but have no trial duration or exposure denominator, so the report counts them separately rather than inventing a per-trial prediction.

`just bench` prints `ok` or `flagged` for each checked file, and every offending class, depth, n, k, interval and mean_p. JSON run records carry the same information in `model_check`, including all eligible groups. A flag says the file does not explain that part of the real evidence; it does not change the comparison verdict or exit status. Investigate the group before using improvements on that file as evidence about the target machine. This is a model diagnostic, not a claim that adaptive journal trials are independent or that all groups jointly have a 99% coverage guarantee.

## Fitting the target machine

```sh
just fit --current-extract --facts tools/bench/facts/target.jsonl.gz --out tools/bench/machines
# Optional: --seed 263 --bootstrap 8 --jobs N
```

`togi-tool-fit` reads the same privacy-safe extract as the model check and requires 16-core profiles. It writes `target-fit-0.json` from all decisive trials and `target-fit-1.json` through `target-fit-8.json` from whole-trial bootstrap samples drawn with replacement. The default seed is 263; refit `n` uses seed `263+n`. Every file declares its extract relative to the output directory and its BIOS context. Mixed-context extracts are refused: split them before fitting. The `target` scenario spreads separate dev and holdout seeds across all nine files, with the replay oracle above each.

Every run names its extract with `--facts`, and every run that writes machine files names its directory with `--out`; neither has a default, so no run reads the committed extract or replaces the committed machines by omission. Without `--current-extract`, an extract holding a session recorded after 20261004T000701Z is refused before anything is fitted or created, and an extract holding any of the original research artifact's seven sessions must be that exact artifact ([model program](../tools/fit/program.md#the-data)). The committed extract therefore needs `--current-extract`, the owner-approved regeneration above; no research campaign passes it. Each report opens with `Facts: sha256 HASH, N rows, N sessions:` and whether the rows are the original research artifact, the hash being the dispatcher's pin of the decompressed rows.

`--jobs N` bounds the whole run's worker pool (default: available CPUs). Output order and machine bytes do not depend on the worker count. The fit's objective, search, parameter bounds, bootstrap constraints and identifiability limits are documented in the `togi-tool-fit` crate (`cargo doc -p togi-tool-fit --no-deps`).

The report identifies constrained refits and their unconstrained flagged groups, intervals and mean probabilities. A constrained ensemble's spread is truncated by the original evidence, not an unconstrained bootstrap confidence interval.

If the all-facts fit itself fails the model check, there is no passing fit to restart from, so `just fit` writes every bootstrap refit as fitted, without constraints, and prints `Refit N written unconstrained` for each flagged one ([ADR 0044](adr/0044-write-flagged-target-fits.md)). The check is unchanged, and a flagged member still cannot support a target-machine claim ([ADR 0029](adr/0029-bench-verdicts-rest-on-fitted-machines.md) Decision 3).

Each generated file's `notes` array records provenance and diagnostics: the generating command, bootstrap index, seed, trial count and raw negative log likelihood, signal-mix failure count, constrained groups and the serialized member's model check against the original extract. A flagged member cannot support a target-machine claim ([ADR 0029](adr/0029-bench-verdicts-rest-on-fitted-machines.md) Decision 3). Each new fit uses per-core mechanisms for R1–R6 and single-core loads and the [shared-voltage model](#in-sample-shared-voltage-anchor) for multi-core R7. It fits no R7 joints or CCD residual. The committed machine files retain their legacy model: the [2026-10-17 frozen score failed](#the-2026-10-17-frozen-score), so no ensemble was regenerated.

A model-check flag is not silently repaired or excluded: inspect the named class, depth and interval before using that ensemble member as target evidence. A flag may reflect a poor local optimum, model assumptions, or a sparse failure absent from a bootstrap sample; it is not by itself proof of an impossible fit. The shared `togi-tool-modelcheck` crate is used by both `just fit` and `just bench`, with the same 99% intervals and unchanged shallowest-loaded-offset grouping.

Treat fits as local optima with sparse-data limits, not discovered hardware boundaries: unsupported/all-passing limits, flat and idle hazards, workload overrides, onset/delay and attribution defaults are qualified in the crate documentation. Multi-core shared-voltage R7 has no unloaded-core hazard. Fixed inputs and seed reproduce output on one host for any `--jobs`, excluding elapsed report lines; cross-architecture last-digit equality is unmeasured. Historical committed files are not a regeneration oracle: [#561](https://github.com/shgew/togi/issues/561) records an unexplained last-digit discrepancy despite same-host equivalence of the old and refactored fitter.


### In-sample shared-voltage anchor

```sh
just fit --shared-voltage-in-sample --current-extract --facts tools/bench/facts/target.jsonl.gz --out tools/bench/machines
```

This retained output mode uses the same fit as the ensemble and forward-only modes, on **all decisive trials**, including record-only facts, and writes only `target-shared-voltage.json`. It requires one nonempty BIOS context and 16-core profiles. The anchor mode rejects `--bootstrap`, `--seed`, `--forward-only` and `--seal`, even when their supplied values equal their defaults. Its command and output file are scheduled for removal only after #480 passes its forward score and regenerates the ensemble.

Before creating the output directory or changing an anchor, it runs the **unchanged model check on the full original extract**, not a bootstrap or selected subset. It reports `ok` only when every eligible group passes (52 groups on the 2026-10-04 extract). A non-ok result prints each exact flagged class, loaded cores, intended duration, depth, observed counts, interval and full-precision mean probability, then exits with failure without writing. A passing in-sample check does not validate later-session predictions.

There is no scalar repair: a failed all-facts check is reported as fitted, never profiled into feasibility. The default ensemble command writes flagged fits under ADR 0044; the retained anchor command refuses to overwrite its output when the unchanged check fails.

Reports print raw total and per-trial in-sample Bernoulli loss for all trials and R7, and predicted/observed R7 failures, even when a flagged fit refuses to write. These are record-only diagnostics, not held-out scores. Generated notes mark the anchor **IN-SAMPLE, NOT forward-validated** and preserve provenance and model-check diagnostics. Unsupported y-cruncher thresholds are not hardware-validated; see the crate documentation for the model's assumptions.

The [Q17 forward bar](https://github.com/shgew/togi/issues/105) remains authoritative: beat the constant predictor on held-out R7 log loss and predict R7 failures within a factor of 2. The [first candidate's failed check](https://github.com/shgew/togi/issues/306#issuecomment-5979625589) was not blind. An all-facts anchor neither supersedes it nor authorizes ensemble regeneration; a future forward claim needs a new blind session.

To replay exact real facts over the anchor, set `"replay": true` on a **bench suite scenario**, not in the machine JSON (that key is unsupported there). For example, place this one-scenario suite at `tools/bench/shared-voltage-suite.json`:

```json
{
  "scenarios": [
    {
      "name": "target-shared-voltage-in-sample",
      "machine": "machines/target-shared-voltage.json",
      "replay": true,
      "dev": [1],
      "holdout": [101]
    }
  ]
}
```

```sh
just bench --suite tools/bench/shared-voltage-suite.json --split dev
```

Machine paths resolve relative to the suite; facts resolve relative to the machine. The declared facts and BIOS context let `togi_tool_trialfacts::Extracts::replay` build a `togi_sim::replay::Replay` for matching decisive trials, with fitted fallback on unmatched classes/profiles. The bench process reads each extract once and shares it between its metric replay oracles and the model checks of every member that declares it; each simulated session's simulator still loads the extract itself. A replay run remains in-sample evidence, not forward validation.

The hand-set shared-voltage scenario, anchor and its two overlays expose partial-load mechanisms, not forward-validated hardware failure rates. Earlier model-check and hazard-ratio measurements describe frozen copies, not today's overlays ([#563](https://github.com/shgew/togi/issues/563)).

### Forward-chained check

After the ensemble's model check, `just fit` orders sessions with decisive trials by their UTC session IDs, comparing equal-second numeric suffixes numerically (`-2` before `-10`). For each session after the first, it fits all earlier sessions' decisive trials once, without bootstrap resampling or model-check constraints, then predicts only the held-out session. These fits stay in memory and do not change the generated machine files.

The prefix fits run in parallel with the same `--jobs` limit; in `just fit` they run alongside the ensemble's fits, and the report prints after the ensemble's. Scoring and pooling remain in session order, so no held-out outcomes enter their own training prefix and the report is unchanged apart from elapsed times.

Each session row names the session ID, ruleset and `training_sessions` count. `trials` and `failures` count held-out decisive trials and observed failures; `predicted` sums the fit's failure probabilities over their intended durations. `log_loss/trial` is the mean Bernoulli log loss, with probabilities clamped to `[1e-4, 1-1e-4]`. It compares the `fit` to a `constant` predictor whose probability, shown in parentheses, is the earlier training trials' failure rate. Lower loss is better: fit loss above the constant means the fit predicts that later session worse than a single average failure rate.

`failures_p<0.01` counts observed failures assigned less than 1% probability: outcomes the model treated as nearly impossible. `exact_matches` is the number and share of held-out trials whose trial class and full profile occur in the earlier training trials. That share bounds what replay could answer from earlier evidence; it does not establish that replay's answers would be correct. `flagged/eligible` counts held-out groups flagged by the unchanged model check, with at least 10 trials and the same 99% binomial intervals. The indented regime rows also show fit and constant log loss over the same regime's trials, using exactly the existing probability clamp and each session's prefix-wide constant.

R7 rows additionally break out each workload (including y-cruncher) and loaded-CCD scope: CCD0 only, CCD1 only, or both CCDs. These scope rows partition the R7 starts; a both-CCD start is not double-counted. Single-core R7 starts also get a separate diagnostic row, while remaining in their workload and CCD totals.

`Pooled` sums all held-out sessions' counts and predictions and divides summed log losses by their total trials. Its constant uses each session's own earlier prefix, not one failure rate fitted to the pooled outcomes; its group counts sum the separate held-out checks. `Forward-chained elapsed` reports the check's own wall time, which in `just fit` overlaps the ensemble's fits. This is evidence about extrapolation to later sessions, not a gate or a guarantee about unseen profiles or other machines.

```sh
just fit --forward-only --facts ARTIFACT            # no ensemble, no machine files
just fit --forward-only --facts ARTIFACT --seal 1   # leave the newest session unfitted and unscored
just fit --forward-only --current-extract --facts tools/bench/facts/target.jsonl.gz
```

`just fit --forward-only` prints the same rows and `Pooled` line, without fitting the ensemble or writing machine files. `--seal N`, accepted only with `--forward-only`, drops the newest N sessions before the check: they are neither fitted nor scored, the remaining rows are unchanged, and `Pooled` covers only the remaining held-out sessions. At least one held-out session must remain. A seal count is not a data permission: on the committed extract, `--seal 1` still scores #480's spent score session 20261004T211726Z, which is why that extract needs `--current-extract` ([above](#fitting-the-target-machine)). Model research ([program](../tools/fit/program.md)) uses only its brief's approved sessions. On the original research artifact the report names the historical confirmation session after the `Facts:` line, as `sealed, neither fitted nor scored` or as `scored, a historical check that is not blind`.

To score the current ensemble beside the forward candidate without refitting its members:

```sh
just fit --forward-only --current-extract --facts tools/bench/facts/target.jsonl.gz --reference-machine tools/bench/machines/target-fit-{0,1,2,3,4,5,6,7,8}.json
```

`--reference-machine` accepts one or more files and requires `--forward-only` and `--current-extract`: the committed members were fitted in-sample to all seven sessions of the original research artifact, the historical confirmation included, so scoring them is never research evidence. Reference scoring uses the same sessions, seal, prefix constants and probability clamp. For each trial it averages the members' failure probabilities before computing log loss, predicted failures and surprises. Its model-check diagnostic is explicitly `flagged_by_any_member/eligible`, not a check of the mean predictor. It is reference evidence, never the candidate's gate.

The report rounds; `--records FILE` also writes the check's full-precision records to the new file `FILE`, one JSON line each: the `input` (the rows' `sha256`, `rows` and `seal`), every scored held-out `trial` keyed by `session` and `seq` (its workload, loaded-core count, outcome, probability before the clamp, constant, both log losses, surprise and match), each held-out `session`'s sums and the `pooled` sums. The losses are sums, added in trial order exactly as the report's, so its rounded means are recomputable. Reference scoring writes no records.

To compare two forward runs, such as a candidate and the unchanged fitter at the reference revision:

```sh
just fit --forward-only --facts ARTIFACT --seal 1 --records candidate.jsonl
just forward-compare candidate.jsonl reference.jsonl --metrics metrics.json
```

`--compare CANDIDATE REFERENCE` pairs the files trial by trial and refuses any pair whose input, seal, trial keys, outcomes, classes or constants differ, so both sides must be prefix fits of the same rows. A reference is the unchanged fitter's own forward run: the committed all-facts ensemble scored with `--reference-machine` is not it. For each held-out session and the pool, and within each for all trials and the single-core, non-R7 and R7 subsets, it prints at full precision the per-trial losses of both sides and the constant, their ratio, `failures_p<0.01`, predicted failures and `|ln(predicted/failures)|`, which a subset without failures reports as none. Each `all` row adds `ratio_95%`, the 2.5% and 97.5% quantiles of the loss ratio over 10,000 paired bootstrap resamples of that scope's trials, both sides of a drawn trial together, from a fixed seed. Pooled sums add per session and then across sessions, as the check does, so they equal the records' own. The report ends with the [model program's](../tools/fit/program.md#the-metric) primary, F1, F2 and F3 checks on the pool. `--metrics FILE` writes the same values to a new file as a flat JSON object of numbers keyed `SCOPE.SUBSET.NAME`, plus `pooled.f3_improved` and `pooled.f3_sessions`, leaving out values JSON cannot hold.

#### The 2026-10-17 frozen score

The [once-only frozen score and extraction provenance on #480](https://github.com/shgew/togi/issues/480#issuecomment-6232034381) and its [raw output](../tools/bench/facts/shared-voltage-score-20261017.txt) are the record, not a recipe to score again. It failed; no ensemble, overlays, anchor or baseline were regenerated and no Ruleset 12 gate was registered. The [owner's decision](https://github.com/shgew/togi/issues/480#issuecomment-6231671066) permits Ruleset 12 to ship ungated and moves #490/#896 to Ruleset 13. Nobody retunes against these spent sessions: a later candidate needs unfitted post-freeze sessions.

The refreshed extract preserves the original research artifact's rows. That is extraction provenance, not validation of the fitted fallback; the [model program](../tools/fit/program.md#the-data) owns the approved research inputs.

### The fit tool (`togi-tool-fit`)

`just fit`, `just fit-artifact` and `just forward-compare` run `togi-tool-fit`. Its modes and flags are described above; its objective, search, bounds and bootstrap implementation live in the crate documentation.

- **The research artifact.** `just fit-artifact FILE` writes the original research artifact to the new file `FILE` (`--write-artifact`): the committed extract's rows of its seven sessions, byte for byte and in extract order, after checking that they hash to the program's SHA-256. It never replaces an existing file, and it fails without writing when the rows differ, for example after a refresh that rewrote those sessions; the extract committed at `455704be` then supplies them.
- **Report.** The elapsed times are the last lines of the report and are not part of the deterministic output; `mean_p` is printed as `1.234560e-2`.

The committed `tools/bench/machines/target-fit-*.json` and `target-shared-voltage.json` stay the Go tool's output until the ensemble is regenerated deliberately.

### The package

`nix build .#default` builds the `togi` binary with crane; it is `packages.default`, which the module installs. It installs the bash, zsh, fish and nushell completion scripts that `togi completion <shell>` generates and stamps `togi --version` with the flake's revision: the short revision, `<rev>-dirty` for a dirty tree, and `dev` when the flake has neither. Its release dependencies are built once and shared with `togi-tool-sim`, so the `sim-suite-<i>` checks and the package reuse them. `just togi ARGS` runs the binary from the tree, which reports the revision `dev`.

The flake check `package` builds the package with the revision `dev`, requires `togi --version` to print `togi <version.txt>+dev`, and requires `togi completion <shell>` to succeed with output that names `togi` for each of bash, zsh, fish and nushell. It fixes no command list, so the commands later pull requests add do not change it. The `module` check and the VM tests take this build from the check's `package` attribute. Reproduce the check with `just check-one package`.
