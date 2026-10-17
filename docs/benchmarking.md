# Benchmarking

`togi-tool-bench` (`just bench`) runs a fixed suite of simulated sessions and compares two versions of togi. Use it to show that a tuner change makes sessions conclude sooner, or no later, without giving up depth or leaving a profile that fails. Like `togi-tool-sim`, it is a development program and is not shipped.

For the autonomous experiment loop, frozen evaluation environment, proof standard and owner-controlled merge and deployment workflow, follow the [research program](../tools/bench/program.md). [ADR 0029](adr/0029-bench-verdicts-rest-on-fitted-machines.md) records why target claims require checked fits and real-fact replay.

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

No gate is registered now: the suite has none until the next ruleset's is committed, so `just bench` prints no gate. The Ruleset 11 gate was scored once, then archived with its baseline, candidate runs and report in [`tools/bench/gates/ruleset-11/`](../tools/bench/gates/ruleset-11/) ([below](#the-archived-ruleset-11-gate)); [ADR 0040](adr/0040-located-hunts.md) records the Ruleset 10 gate.

A gate's `notes` array preserves accompanying provenance and caveats as JSON data; it does not affect scoring. `just bench` prints each `notes` entry of a registered gate as a `suite: ...` line before its summary.

With `--baseline`, `just bench` judges the gate after the generic verdict below, prints each criterion with its estimate, interval, threshold and `PASS` or `FAIL`, then an overall `gate ID: PASS` or `FAIL`, and exits 0 only when the gate passes. Run it as `just bench --split all --baseline tools/bench/baseline.jsonl`, scored once on every seed of the gate's split; a different `--split`, a candidate or baseline run missing for a required seed, a baseline recorded at another ruleset or commit than the gate names, or a missing `worst_r7_hazard_per_h` fails the gate with a refusal that names the cause. A suite without a `gate` prints no gate and exits as before. The criteria, per scenario unless noted:

- **Conclusion:** every seed the baseline concluded is still concluded by the candidate, on each gated and each `conclude` scenario.
- **Median and 90th percentile** (linear interpolation) of the final-profile `worst_r7_hazard_per_h`: fails only when the `confidence` interval of the change lies entirely above 0. The change is the candidate's quantile minus the baseline's, not the quantile of per-pair differences. The interval resamples seed pairs `resamples` times, drawing from a `StdRng` seeded with `bootstrap_seed` ([Judging the gate](#judging-the-gate)), so a run always prints the same interval. Over 24 seeds the interval is wide, so the 90th-percentile criterion has low power: it catches a large tail rise, not a small one.
- **Time:** the geometric mean over pairs both concluded of candidate `sim_hours` over baseline's is at most `max_time_ratio`, with no exemption.
- **Pooled median:** the median over the `pooled` scenarios' seeds together is strictly lower than the baseline's; equal fails.

Maxima and everything else are reported by the summary and the comparison, not gated. Treat a failed gate as the owner's decision, not a threshold to renegotiate after scoring.

### The archived Ruleset 11 gate

[`tools/bench/gates/ruleset-11/`](../tools/bench/gates/ruleset-11/) keeps the Ruleset 11 gate as it was scored, once, at commit `c7e8794a`, the clean commit of the Go build that bumped the ruleset to 11:

- `suite.json` is byte-identical to the suite that registered the gate, scenarios included. Its machine paths are relative to `tools/bench/`, so they do not resolve from the archive; the gate only reads its seeds.
- `baseline.jsonl` is the Ruleset 10 baseline the gate was judged against (220 runs at `3a9b894` and `21052983`).
- `candidate.jsonl` holds the 220 scored runs, projected like the baseline (no `model_check`, `wall_s` or `partial_seconds`).
- `report.txt` is the output as scored, less its first line, the dev shell's fetch notice, which named a local path.

`the_archived_go_gate_is_kept_unchanged_and_never_re_scored` (`crates/togi-tool-bench/tests/gate.rs`) pins the four files' hashes and checks that the bench refuses the archive ([below](#judging-the-gate)); the Go bench's judgment is kept as `report.txt`, not recomputed.

**Result: FAIL, 5 of 21 criteria.** The median worst R7 hazard rose on `target-shared-voltage`, `target-r7-vf-boost` and `target-r7-request-gap` against Ruleset 10 (3.94, 3.31 and 3.17 against 1.60, 1.70 and 1.88 per hour, each interval above 0), so did the 90th percentile on `target-shared-voltage` (6.73 against 2.64), and the pooled median is higher (3.31 against 1.72). All eight conclusion criteria and all four time criteria passed: all 220 runs concluded.

The owner granted an exception ([comment](https://github.com/shgew/togi/issues/530#issuecomment-6102890949)) on the absolute bars of [#493](https://github.com/shgew/togi/issues/493) Q24, worst R7 hazard median and p90 not above Ruleset 9's final 4.91 and 10.90 per hour, judged on the point estimates; all pass. The registered criteria compare against Ruleset 10, whose hazard Ruleset 11 does not keep. The gate did not pass and is recorded as FAIL.

Simulated `target-shared-voltage`, 24 seeds, against the Ruleset 10 baseline:

| | Ruleset 10 | Ruleset 11 | Target (#493) |
|---|---|---|---|
| Median hours to conclusion | 249.8 | 66.5 | about 48 |
| 90th percentile hours (linear) | 390.8 | 70.7 | about 72 |
| Median hours to first confirmed result | 61.6 | 48.6 | 33–43 |
| Median crashes | 426.5 | 202.5 | below 426.5 |
| Worst R7 hazard, median per hour | 1.60 | 3.94 (interval of the change +1.13 to +2.96) | not above 4.91 |
| Worst R7 hazard, 90th percentile per hour | 2.64 | 6.73 (interval of the change +2.40 to +6.87) | not above 10.90 |

The conclusion time misses its ballpark by about 18 hours and the first result by about 6; the ballparks are not gates ([#493](https://github.com/shgew/togi/issues/493) Q28). These are simulated results, not hardware measurements.

### Historical measurements

The **historical score of 2026-10-07**, recorded in [ADR 0040](adr/0040-located-hunts.md), used the anchor merged at `0ccbe089` (generated at `0f6b5d23`), before its signal-mix regeneration in [#461](https://github.com/shgew/togi/pull/461), merge `bf7230d`. Those gate numbers are simulated results on that earlier evaluation environment, not current-anchor timings.

On the regenerated anchor (`18a51a63`, merged at `bf7230d`), the [2026-10-07 #493 measurements](https://github.com/shgew/togi/issues/493#issuecomment-6040930168) report **simulated** median total time of 53.1 h for ruleset 9 and 249.8 h for ruleset 10, about 4.7×, on `target-shared-voltage` with 24 seeds. Ruleset 10 ran at `645f529`; ruleset 9 ran at `84c2a01` with the same simulator and regenerated machine. That anchor remains unchanged at `2f318490` (2026-10-10). The earlier 1.90×/2.03× claims do not describe it. See [How togi tunes](how-togi-tunes.md) for the measured phase breakdown and [the reproduction recipe](#reproducing-an-older-ruleset).

Where these measurements and the Ruleset 9 and 10 numbers name `target-r7-vf-boost` or `target-r7-request-gap`, they describe the earlier frozen copies, not today's overlays.

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

```sh
just audit                                        # every suite scenario, 200 seeds each; keeps a temporary evidence directory
just audit --seeds 50 --jobs 8 --keep runs          # change the sweep size, parallelism and retention directory
just audit COPY-OF-STATE-DIR OTHER-STATE-DIR         # read-only audit of current and archived journals
just audit --records RECORDS.jsonl --root BENCH-DIR # re-audit a retained bench sweep, including wall times
```

### The audit (`togi-tool-audit`)

`just audit` runs `togi-tool-audit`:

- The sweep runs `togi-tool-bench` (found beside the audit executable, or `--bench FILE`) with `--split dev --keep` over a generated suite, and the bench builds the simulator (`--sim FILE` names another). `--timeout` is in seconds (default 1800). Without `--keep`, the sweep runs in a temporary directory that is removed once it is audited and when the bench fails, so a default `just audit` leaves nothing behind; a failure message then says the evidence was removed and to rerun with `--keep DIR`. SIGINT, SIGTERM and SIGHUP stop the bench and remove the temporary directory (a kept one stays), print `interrupted` and exit 1. The default sweep's temporary directory is made under `$TMPDIR` and holds every run until the audit ends: about 8 MB per run on the measured 20-seed sweep (1.8 GB for 220 runs; the size is that of the final files, and the sweep was measured before [#868](https://github.com/shgew/togi/issues/868)'s fix, when each run's `state.json` was rewritten after every event), and on the order of 18 GB for the default 200 seeds, by extrapolation and not measured. Point `TMPDIR` at a disk with room, or pass `--seeds` or `--keep`. With `--keep DIR`, the evidence directory `sweep-*` under DIR retains `suite.json`, `bench.log`, `records.jsonl` and the bench's `bench-*` run root. The generated suite is the suite's JSON with the gate, holdout and smoke seeds dropped and `dev` set to 1 through `--seeds`; the bench validates it, so a suite the bench rejects fails the sweep with the bench's message.
- The report is JSON Lines on stdout, one object per violation with `directory`, `scenario` and `seed` (for a bench run), `journal` (`live` or an archived session id), `seq`, `check` and `reason`; `check` is a journal check of [Invariants](spec/tuner.md#invariants), `wall_time` (over ten times the scenario's median), `bench_status` (a bench status other than `concluded` or `deadend`, so a boot-cap `censored` run is one) or `unreadable`. The sweep checks that every dev session concludes within its boot cap. [#546](https://github.com/shgew/togi/issues/546), still open, owns auditing capped runs as unfinished prefixes, together with moving the checks into the bench. The summary line `directories= journals= violations=` and the evidence paths go to stderr. Exit status 1 means a violation or a failure, 2 a usage error.
- The audit takes each directory's writer lock, so a directory a tuner is running is refused. Taking the lock creates the directory's `lock` file and sets its mode to 0711, so audit a copy. A bench run's live journal must also have concluded and have a `state.json` at its last event.
- The audit reads journals leniently ([Journal](spec/journal.md)): an event of a kind this build does not know is left out, a cause that names it is not a bad cause and the gap it leaves is not a sequence violation. Each journal that lost anything is named on stderr (`togi-tool-audit: "DIR" JOURNAL: ...`) before the summary; stdout and the exit status are unchanged. This holds for the sweep too, whose journals this build wrote, so the sweep no longer refuses an unknown kind.

The sweep composes `togi-tool-bench --keep` with a generated suite: seeds 1 through `--seeds` for each scenario, preserving ensemble selection and replay settings. `--suite FILE` chooses another suite; sessions are bounded by simulated boots, not wall time. The printed evidence directory retains the generated suite, bench log, run records and state directories.

The audit checks recorded failure-point and combination avoidance independently of the tuner, SMU intent/write/readback ordering, the offset range ([Invariants](spec/tuner.md#invariants); readbacks and baselines report the hardware and are not bounded), causal references, simulated-run conclusions, and exact `state.json` replay when the snapshot's `last_seq` matches the replayed journal. A multi-core R7 failure naming a core at CO 0 records a failure point only when a dead end cites it ([R7 request order and attribution](spec/tuner.md#r7-request-order-and-attribution)). A snapshot at another sequence is stale and is skipped: a crash between the journal fsync and the state write can leave it behind by design. A session from another ruleset has no current projection and is skipped too. So is a session started by an older journal schema: replay always stamps the current schema, and the next run archives that session. Likewise, a `state.rebuilt` event is a violation only when its differing `fields` omit `last_seq`, meaning a current snapshot disagreed with replay, and the run's next `config.loaded` records the same build as the previous one; a changed build may project fields an older snapshot lacks, which is a legitimate upgrade. A rebuild whose run ends without a `config.loaded` (its shutdown, dead end, session archive or a later boot, or the end of a finished simulated journal) is a violation too; one at the end of a live journal is deferred. Real journals may still be live, including between a write and its readback. A write followed by a reboot or the end of an archive is an interrupted action, and a same-boot resume's readbacks of every core complete it; archived transitions and reset boundaries are valid session endings. Bench records additionally flag runs exceeding ten times their scenario's median wall time. Every violation is a JSON object on stdout with its journal, session, sequence and reason; summaries go to stderr, and any violation exits nonzero. Audit copies of real state directories so the journal and state projection are a consistent snapshot.

## Proving unchanged decisions

For a shape-only change, `just same [base]` proves that decisions are unchanged on simulated paths only: the simulator stands in for `togi-trial`, `togi-detect`, `togi-smu`, `togi-hardware` and the backends, so it proves nothing about a change to them ([what each check proves](#what-each-check-proves)). The base defaults to `origin/main` and is exported locally without fetching or registering a worktree. It builds the base and current simulators with `cargo build --release --locked` and runs each tree's own suite and machine files across every dev and holdout seed. Sessions pair by scenario, split and seed. Every current and archived journal pairs by its relative path and is compared byte for byte, after removing only `version`, `rev` and the exact build description in `msg` from `session.start` and `config.loaded`. Everything else, including `ruleset`, `schema`, `fixes` and `evidence_epoch`, must match.

Each side's run is reduced to a digest of its normalized journals and its exit status, which the command caches in the user cache directory (`togi/bench` under `$XDG_CACHE_HOME` or `~/.cache` on Linux, `~/Library/Caches` on macOS) under a key made of the simulator binary's SHA-256, `--max-boots` and the sessions with the machine and facts files they read. The binary carries the code, the toolchain and the platform, so a base that does not change the simulator is a cache hit across commits, and a rerun after an edit that leaves the simulator unchanged (a comment, a doc, a test) runs nothing. When both trees build the same simulator and read the same inputs, the command says so and reports no difference without running any session. Every other session runs once per side that lacks a record, longest first, as independent jobs, and each run directory is removed as soon as its digest is taken and its pair compared. Only the cache keeps what a run proved; the cache keeps the eight most recently used keys.

A difference stops the comparison: the remaining runs are cancelled and the first difference prints. `--keep-going` runs every session and counts every difference. A differing session prints its first differing event's journal path, line number and both normalized lines; its run directories are retained and their directory printed on stderr, after rerunning any side that was cached or already deleted. Missing sessions or journal files and differing simulator exit codes also count as differences. The command exits 0 when all sessions match and nonzero on a difference, execution error (a simulator that exceeds the hang safety limit included) or invalid arguments. A difference is fixed or split into its own issue, never explained away. A session that differs only because of a stale cache entry shows `<journal digest>`; rerun with `--no-cache` to rule that out.

Every session's wall time is part of its cached record, so the report compares head with base even when the base ran in an earlier invocation. It prints the total of both sides and every session whose head took more than twice its base's time. That is informational: it never changes the verdict or the exit status.

`just smoke [base]` is the same comparison, with the same cache, fail-fast and flags, over only the sessions the suite lists under `smoke`: a quick check while editing, to run `just same` after. A session the base suite lacks counts as a difference.

This is not a CI check: it needs two builds. `just bench` cannot prove equality because it compares metrics against thresholds; one changed decision with unchanged totals can pass.

To control concurrency, caching or retention directly, run `togi-tool-bench same` with its own flags, described [below](#the-same-command).

## The suite gate

CI runs every session of `tools/bench/suite.json`, dev and holdout alike, on the simulator (`togi-tool-sim`, [Simulating](simulating.md#the-simulator-command-togi-tool-sim)) as the flake checks `sim-suite-0` to `sim-suite-<N-1>`. `nix/sim-suite.json` declares N. Each check builds the release `togi-tool-sim`, runs its shard of the suite (the partition gives shard sizes that differ by at most one) with one session per core, and then checks its manifest against exactly the run ids the partition gives that shard. A session passes only when it is `clean`:

- it stopped for its clean cycle: `shutdown` reason `cycles`, never a dead end, a signal or the 1000-boot cap;
- its resume state matched a fresh fold of the journal after every crash and at the end;
- `togi_tuner::replay` folds its whole journal;
- `togi-tool-audit` finds no violation in any journal of its state directory, and its published `state.json` equals a fresh fold of the live journal.

The aggregating `check` job reads N from `nix/sim-suite.json` and fails unless the build matrix held exactly `sim-suite-0` to `sim-suite-<N-1>`, so a deleted or renamed shard fails the required check rather than vanishing from the matrix. The suite declares no expected dead end, and none is to be invented to pass the gate. `just sim-suite` runs the same suite locally in one shard and checks it; [`togi-tool-sim`](simulating.md#the-simulator-command-togi-tool-sim) also has a test that splits one session across processes by `--max-boots` and requires the same journal and `state.json` as an uninterrupted run.

`just gate` leaves the shards out; CI and `just check` on Linux run them. `just sim-suite` reproduces the whole suite, and `just check-one sim-suite-<i>` reproduces shard `i`. Raise N in `nix/sim-suite.json` whenever a shard's CI job (`completedAt − startedAt` in `gh run view --json jobs`) is slower than the slowest job of the same run that is not a shard. That keeps the suite within [#627](https://github.com/shgew/togi/issues/627)'s limit that a job is never slower than the current slowest one. The `build` job's `timeout-minutes: 45` stops a session that never concludes, instead of holding a runner for six hours.

This gate is separate from `just bench`: it proves that every session concludes cleanly, not that a tuner change concludes sooner or keeps its depth.

## What each check proves

Cite a check as evidence only for the packages it exercises. `just gate` runs `fmt`, `lint`, `module` and `changes`, the retained Go tools' tests, and the workspace's nextest and doc tests, `cargo deny` and `cargo shear`; `just check` runs every flake check the host builds, including the Linux-only `trial-scope-tests`, `vm-recovery-tests`, `vm`, `vm-trial-scope`, `vm-restart-limit` and `vm-emergency-stop`.

| Check | Runs | Exercises | Does not prove |
|---|---|---|---|
| `just test`, `just focus` | `cargo nextest run` on the workspace or the named crate | Every crate's ordinary tests: the tuner, session, carry and journal directly, and trial, detect, smu, hardware and the backends against fakes, fake file trees and fake package trees | That a stress program accepts its generated configuration; real systemd scopes, kernel logs or SMU access |
| `go-tools` | `go test -shuffle=on -tags integration ./...` in `just gate` and the flake `go-tools` check | The retained Go tools' tests, among them `tools/cover` against temporary git repositories | Any Rust crate, real systemd scopes, stress programs, kernel logs or SMU access |
| `board-test`, `just board-test` | Nushell assertions with fixed GitHub JSON and time, in `just gate` and the flake check | Coordination-board readiness, grouping, links, overlaps, interaction expiry, failures and golden text | Live GitHub API availability or authentication, any togi runtime code or hardware |
| `module` | Evaluates `nix/module.nix` for x86_64-linux on any host (`nix/module-test.nix`), with the package built with the revision `dev` (`package`'s `package`), which the evaluation references without building it | The module's options and generated configuration: GRUB mirror constraints, the grubenv mount dependency, the tuning specialisation's 30 s watchdog and kernel parameters, the restart-limit and leave-tuning-boot scripts, the lock file rule and package overrides | Any code of togi, or that the configuration boots and behaves as evaluated |
| `trial-scope-tests` | Linux only: builds `togi-trial`'s `hardware` and `cached_errors` test binaries with the `hardware` feature, as `togi-trial-tests` and `togi-trial-cached-errors` | That those tests compile | Anything at run time: `vm` and `vm-trial-scope` run the binaries |
| `vm` | Linux only: a NixOS VM with the module, whose `togi` is the package built with the revision `dev`, and its tuning specialisation (`nix/vm-test.nix`) | `togi-trial-tests`'s `hardware::` tests, except the helper process and the R1 and R2 workload test, as root under real systemd and its pin test as a delegated user, with helper backends; the documented lock provisioning; the installed `togi`'s host-lock refusals, its dead end at preflight on the tuning boot and the saved entry it clears; the watch and console services; booting the GRUB tuning entry; the armed watchdog's reset when PID 1 freezes | Real stress programs, SMU access or offset writes, a passing preflight, or any tuning decision |
| `vm-trial-scope` | Linux only: a NixOS VM with the module's tuning node, running `togi-trial-tests` and `togi-trial-cached-errors` as root and as a delegated user | The scope tests that need real systemd, as root and, for the delegated subset and `cached_errors`, through the user manager, with helper backends, leaving no `togi-*` scope behind | Real stress programs or SMU access, or the installed `togi` |
| `vm-restart-limit` | Linux only: a NixOS VM whose `togi` is a stub that exits 2 for `run` and `restart-limit` | The module's units and scripts: three failed starts, the start limit, the fallback leave reason, the return to the normal generation, the cleared GRUB saved entry and the lock file's ownership | Any code of togi itself, including its own `restart-limit` command |
| `vm-recovery-tests` | Linux only: builds `togi`'s `vm_recovery` integration-test executable as `togi-vm-recovery` | That the production CLI recovery path and its persistent-fake-machine fixture compile | Runtime recovery or real SMU access |
| `vm-emergency-stop` | Linux only: a NixOS VM using the module's service, the production emergency-stop CLI on a persistent fake SMU, and the real trial runner with sleeping helper workloads | Killing the controller mid-trial, every-core zero readback and removal of real systemd trial scopes before the unchanged 60-second restart; controller affinity during the trial, clean-stop behavior and emergency validation/sweep/readback failures | Real SMU access, real stress programs or the production session's tuning decisions |
| `just same` | Both trees' `togi-tool-sim` across every dev and holdout seed of their suites, with journals compared byte for byte (above) | The session, tuner, carry and journal on the paths the suite's scenarios reach, through the simulator's SMU, host, trials and kernel | Any change to trial, detect, smu, hardware or the backends, which the simulator replaces; paths no scenario reaches |
| `just smoke` | One session per scenario of both trees' `togi-tool-sim`, the suite's `smoke` list, compared as `just same` does | The same simulated paths as `just same`, on the sessions with the most crashes and longest histories of each scenario | Anything `just same` does not prove, and the paths the other sessions reach: run `just same` before relying on it |
| `just bench` | Simulated sessions for the chosen split (dev by default) and the model checks; with `--baseline FILE`, a comparison with that baseline by metric thresholds and, when the suite registers one, [the gate](#the-gate) | The same simulated paths as `just same` | Unchanged decisions (one changed decision with unchanged totals can pass), or anything about the packages the simulator replaces |
| `sim-suite-<i>` | Shard `i` of the 220 suite sessions on the release `togi-tool-sim`, as [above](#the-suite-gate); the `check` job requires every shard `nix/sim-suite.json` declares | The session, tuner, carry, journal and simulator on the paths the suite reaches: each session concludes, resumes warm state equal to a cold fold, replays through the tuner and passes the invariant audit | Better or equal tuning (bench scoring does that), or anything about the packages the simulator replaces |
| `package` | Builds the release `togi` with the revision `dev` and runs it: `togi --version` and `togi completion` for bash, zsh, fish and nushell, as [below](#the-package); the retained Go tests belong to `go-tools` | That the package builds and installs, prints `togi <version.txt>+dev` and generates a completion script that names `togi` for each shell | What any command does, the set of commands, or that a completion script loads in its shell |
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

The ensemble is the suite's `target` scenario with all its dev and holdout seeds, each run on its fitted member with real-fact replay, so new members join the forecast unchanged. For each run, the tool copies the given directory's `events.jsonl`, `state.json` and every archive entry the journal reads sessions from (archived `archive/*.jsonl` journals, the `archive/*-carry-pending` and `archive/*-compat-pending` markers of an interrupted transition, and `archive/*-reset-all` markers) into the run's own directory and resumes that copy with `togi-tool-sim` until it concludes, as `togi run --cycles 1` would: under the configuration the copy's latest `config.loaded` recorded, with its backend store paths, durations, evidence and checking cycle, so the passes on record keep counting. A live journal of an older schema contributes only its backend store paths, on top of the default configuration ([simulating](simulating.md)). That recorded snapshot stands in for the configuration file the real machine would load, so a forecast matches the next real run only while that file is unchanged since the recorded `config.loaded`; the given directory is only read. An archive entry of those names that is not a regular file, such as a symlink, fails the forecast with an error naming it.

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

The output and the record are identical for a fixed copy, commit, suite, machine files and facts extract: they hold no wall times or local paths. `--out FILE` writes the record as JSON. Name it `tools/bench/forecasts/<session>-<seq>.json` after the anchor and commit it in a pull request that merges before the run starts, so the history shows that the forecast predates the run and anyone can score it again with `just stats`. The record holds the anchor's identity and simulated outcomes only, no journal content.

The crate `togi-tool-forecast` defines the record, which `togi-tool-bench forecast` writes ([below](#the-forecast-command)) and `stats --forecast` ([#820](https://github.com/shgew/togi/issues/820)) scores. It holds the anchor; the short commit, a dirty flag and the ruleset; the evidence epoch; `files`, each machine file and facts extract the ensemble read, by path relative to the suite and SHA-256; each run's machine, seed, split and `outcome`, a nested object (status, hours, crashes, hunts, the profile as an object of core to offset, and the depth); and the summary of statuses and ranges, `summary.cores` an object of core to range. It hashes the anchor session's journal lines as the file holds them. The crate refuses a record in another layout, a Go-written one included, and an array or an unknown key where it expects an object. The build reads no Go-written journal, state or archive ([Reset and compatibility](spec/runtime.md#reset-and-compatibility)), and likewise no forecast the Go tool wrote.

`--split`, `--baseline` and `--same` are usage errors with `--forecast`. `--keep DIR` retains every run directory under a new directory in DIR. Without it, each successful run is deleted as it finishes; a forecast whose runs failed keeps the failed runs and prints where, and a forecast that fails after every run succeeded, such as on writing `--out`, removes its run directory and prints none.

## The bench (`togi-tool-bench`)

`just bench` runs `togi-tool-bench`: the runs, metrics, summary, comparison and cache of the bench ([#821](https://github.com/shgew/togi/issues/821)). `just same`, `just smoke` and `just forecast` run its `same` and `forecast` commands ([#823](https://github.com/shgew/togi/issues/823)), described [below](#the-same-command). The gate report is [below](#judging-the-gate) ([#822](https://github.com/shgew/togi/issues/822)).

```sh
just bench --split dev --baseline BEFORE.jsonl --out AFTER.jsonl
just bench sanity --records AFTER.jsonl
```

The suite is `tools/bench/suite.json`, read by strict rules (unknown keys refused, a machine file per scenario, a replay oracle for `replay: true`); every scenario's machines and replay oracle are loaded and checked whatever `--split` selects. A run builds `togi-tool-sim` from this tree with `cargo build --release --locked`, copies the binary into a private directory, and starts that copy as a subprocess for every run with a temporary `--state-dir` whose journal the bench reads after it exits ([ADR 0060](adr/0060-bench-children-keep-temporary-journals.md)), in a process group of its own, with the flags `just sim` takes; `--sim PATH` copies and runs a given binary instead. `--jobs` (default: the CPU count) bounds the simulators at once, `--timeout SECONDS` is the hang safety limit of one run (an overrun is a harness error, not a run outcome), and `--max-boots` (default 1000) is the boot cap that makes a run censored. A harness error stops the simulators still running and is what the bench reports. A session a signal killed or stopped is an `error` record and is not cached. `--keep DIR` retains every run directory and skips the cache. The records come out in suite order whatever `--jobs` is, so they do not depend on scheduling. The bench records the tree's commit and dirty flag, so it runs inside a Git work tree. Ctrl-C stops the simulators that are running, discards kept run directories and exits non-zero.

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

`just forecast STATE_DIR [--out FILE] [--suite FILE] [--keep DIR] [--jobs N] [--max-boots N] [--timeout SECONDS] [--sim FILE]` runs `togi-tool-bench forecast`, which does what [the forecast above](#forecasting-a-real-run) describes with the simulator: it copies the directory's `events.jsonl`, `state.json` and its session-bearing archive entries (`archive/*.jsonl`, `*-carry-pending`, `*-compat-pending`, `*-reset-all`; any other kind, such as a symlink, fails the run) into each run's own directory, resumes the copy on `togi-tool-sim` for every dev and holdout seed of the suite's `target` scenario, and counts only the events after the anchor. The copy is read, never written. An exit 1 is a dead end only when the run reached one after the anchor: a `deadend` event after it, or the simulator's own `sim: dead end` log line; a dead end the copy already held is not the run's. An exit that is neither a conclusion, a dead end nor the boot cap, or a simulator that records no event after the anchor, fails the forecast.

The record is `togi-tool-forecast`'s `Record` with the commit and the dirty flag the command reads from git in its working directory, the ruleset and evidence epoch of this build, `files` (each machine file and facts extract the ensemble reads, by path relative to the suite, with its SHA-256, in the order the machines list them: a machine file before the files it extends) and the runs. The printed report names the anchor, the commit (with `(dirty)` when the tree has changes other than `--out`), the ruleset and the runs by status, then a table of each core's final offset, the depth, hours, crashes and hunts as median, 10th to 90th percentile and range, and the target machines' model checks. It holds no wall time and no local path: a machine file is named by its path relative to the suite's directory, in the record and in the model checks, however the suite names it, and a file outside that directory is refused. A failed run stops the simulators still running, and the forecast names the runs that failed, not those it stopped. A forecast whose runs failed keeps the failed runs' directories and prints where; one that fails later, on writing `--out`, or is interrupted keeps nothing; `--keep DIR` keeps every run, an interrupted one's directory included. There is no flag that conflicts with `forecast`: `--split`, `--baseline` and `--same` do not exist for it.

## Checking a machine against real evidence

A machine file may declare `"facts": "../facts/target.jsonl.gz"`, resolved relative to the machine JSON file. Every `target-fit-*.json` declares the committed target-machine extract. Files without `facts` are not checked.

Regenerate the extract from a temporary copy of the state directory, never the live state:

```sh
just facts COPY-OF-STATE-DIR
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
just fit
# Optional: --facts EXTRACT.jsonl.gz --out DIRECTORY --seed 263 --bootstrap 8 --jobs N
```

`togi-tool-fit` reads the same privacy-safe extract as the model check and requires 16-core profiles. It writes `target-fit-0.json` from all decisive trials and `target-fit-1.json` through `target-fit-8.json` from whole-trial bootstrap samples drawn with replacement. The default seed is 263; refit `n` uses seed `263+n`. Every file declares its extract relative to the output directory and its BIOS context. Mixed-context extracts are refused: split them before fitting. The `target` scenario spreads separate dev and holdout seeds across all nine files, with the replay oracle above each.

The independent fits, candidate scoring within each fit and the forward-chained check share one Rayon pool, bounded by `--jobs` (default: the available CPU count). Unconstrained fits finish before any flagged bootstrap members restart from a passing all-facts fit; those constrained refits also use that pool. `--jobs 1` uses one worker for fits, candidate scoring and the forward-chained check. Concurrent scores are summed in a fixed order, so machine bytes and report order do not depend on the worker count or the CPU count.

Each bootstrap sample is fitted without constraints first. If its model check against the original extract flags a group and the all-facts fit passes that check, the fitter restarts from the all-facts fit and minimizes the **same resampled augmented objective**, accepting only parameter moves, including shared-voltage moves, that remain inside every original group's unchanged 99% interval. It does not redraw the sample, tune the seed, change the interval or repair the observed counts. This is a **checked, constrained bootstrap ensemble**: its spread is truncated to what the real facts admit, not an unconstrained bootstrap confidence interval. The fit output identifies each constrained refit and its flagged groups, including the unconstrained interval and mean probability.

If the all-facts fit itself fails the model check, there is no passing fit to restart from, so `just fit` writes every bootstrap refit as fitted, without constraints, and prints `Refit N written unconstrained` for each flagged one ([ADR 0044](adr/0044-write-flagged-target-fits.md)). The check is unchanged, and a flagged member still cannot support a target-machine claim ([ADR 0029](adr/0029-bench-verdicts-rest-on-fitted-machines.md) Decision 3).

Each generated file's `notes` array records provenance and diagnostics: the generating command, bootstrap index, seed, trial count and raw negative log likelihood, signal-mix failure count, constrained groups and the serialized member's model check against the original extract. A flagged member cannot support a target-machine claim ([ADR 0029](adr/0029-bench-verdicts-rest-on-fitted-machines.md) Decision 3). Each new fit uses per-core mechanisms for R1–R6 and single-core loads and the [shared-voltage model](#in-sample-shared-voltage-anchor) for multi-core R7. It fits no R7 joints or CCD residual. The committed machine files retain their legacy model: the [2026-10-17 frozen score failed](#the-2026-10-17-frozen-score), so no ensemble was regenerated.

Before the telemetry refresh, seed-263 refits **2 and 8** required the constraint on `R7 mprime-avx2-36k-248k-allcore`, cores 0–7, 120 s, depth -24 (`n=45`, `k=5`). Their unconstrained predictions were respectively `mean_p=0.0178977`, interval `[0,4]`, and `mean_p=0.0125439`, interval `[0,3]`. That ensemble reported `ok` on its then-current 44 eligible groups. These historical numbers do not validate regenerated machines against the refreshed evidence; use the current `just fit` model-check report.

Against the 2026-10-04 extract, the legacy all-facts fit itself fails the model check: on `R7 mprime-avx2-36k-248k-allcore`, cores 0–7, 300 s, depth −24, it predicts `mean_p=0.0718896`, interval `[0,6]`, against 7 failures in 24 trials. The committed `target-fit-*` files are therefore unconstrained. Each member flags that group, the same workload's cores 8–15, 120 s, depth −39 group (13 failures in 38 trials), or both, and its `notes` name which. Those historical diagnostics are not a check against the 2026-10-17 refresh; none of the nine supports a target-machine claim until a fit passes the unchanged check.

Every all-facts fit, whole-trial bootstrap refit and forward-prefix fit uses one fitting path and one objective: Bernoulli negative log likelihood plus the shared-voltage threshold shrinkage, margin prior and named-core attribution term described below. Every coordinate search and sweep acceptance uses that objective; reports print only the raw Bernoulli loss. Predictions use `togi_sim::Model::failure_probability` over each trial's **intended**, not measured, duration, exactly as the model check does. Equal profile/class observations are aggregated without losing counts, while bootstrap trials carry their request/clock telemetry and named-core evidence with them. A bounded coordinate search fits per-core alone/together limits, workload overrides, hazard shape, supported flat and idle mechanisms, and shared-voltage parameters. It is a local optimum, not a guarantee of a global optimum.

Each sweep also searches coupled integer limit/rate shifts. Moving active per-core, workload and idle limits by `delta` and scaling the past-limit rate by `growth^(-delta)` preserves already-past-limit hazards until a profile crosses a limit. This lets the search remove false hazards at observed clean boundaries instead of getting stuck when separate limit and rate moves each worsen the objective. Unsupported -50 limits stay fixed, and the same rate/limit bounds and original-evidence constraints still apply.

The positive-rate search covers past-limit rates from `1e-8` to `0.5` failures/s, near-limit rates from `1e-8` to `0.001`/s and flat rates from `1e-10` to `0.001`/s. Growth ranges from `1.05` to `8`. Zero is also considered for past-limit, near-limit and flat rates. Limits range from -50 to 1, matching the simulator's stock-failure representation.

The simulator still reads legacy joints and the `ccd` residual in the committed machines and hand-written scenarios. They are not fitted by the shared-voltage path; the residual format remains only until the approved ensemble regeneration.

The extract cannot identify every simulator parameter. Unsupported or all-passing limit boundaries stay at -50; absence of failures does not prove that boundary. The fit shares the hazard shape across cores and regimes because sparse failures cannot resolve a separate shape for each. Unsupported workload overrides stay absent; flat rates stay zero without supporting failures. Onset and delays stay at zero: decisive binary trials do not identify failure-time shapes. Crash-MCE probability, bank attribution and reset kinds retain simulator defaults; these are not parameters of the binary likelihood. Idle facts without trials are counted by the check but provide no idle exposure denominator. Idle limits require decisive trials with nonzero offsets on unloaded cores. Flat and idle mechanisms remain available outside multi-core shared-voltage R7; that path suppresses them so unloaded offsets cannot change its hazard.

Each fit counts the recorded signals of its sample's failures: `model.signals` holds pooled counts, and `model.regime_signals` the counts of each regime with failures. A signal the sample never recorded gets no weight, and a regime without failures draws from the pooled counts. An uncorrected machine check counts as a crash, which the simulator draws as a crash that leaves an MCE with probability `crash_mce`; a failure without a recorded signal is not counted. A sample without any recorded failure signal keeps the simulator's default weights. Shared-voltage named-core attribution also contributes to the fitting objective; the forward and model checks score only binary outcomes.

Generation prints raw likelihoods, fitted signal counts per regime, model checks against the **original** extract (also for bootstrap fits), and elapsed wall time. Files contain no timestamps and use stable ordering and full-precision parameters, so fixed inputs and seed reproduce them byte for byte on one host for any `--jobs` (`crates/togi-tool-fit/tests/fit/determinism.rs`). No test compares a fresh generation with a committed machine file. Cross-architecture last-digit equality has not been measured; compare decoded values instead. The committed files remain the Go fitter's output until deliberate regeneration.

**Recorded discrepancy.** The original and the refactored fitter ([#561](https://github.com/shgew/togi/issues/561)) write byte-identical files from the same inputs on the same amd64 host, yet both differ from the historical committed fits in 232 last-digit scalar values across ten files: 72 in the nine `target-fit-*` files and 160 in the shared-voltage anchor. The cause is unknown; a different architecture is only an inference. The header split re-encoded the existing decoded configs with new notes and refit nothing, so every numeric value is exactly the historical one, and this change does not claim that the historical fits are reproducible.

A model-check flag is not silently repaired or excluded: inspect the named class, depth and interval before using that ensemble member as target evidence. A flag may reflect a poor local optimum, model assumptions, or a sparse failure absent from a bootstrap sample; it is not by itself proof of an impossible fit. The shared `togi-tool-modelcheck` crate is used by both `just fit` and `just bench`, with the same 99% intervals and unchanged shallowest-loaded-offset grouping.

### In-sample shared-voltage anchor

```sh
just fit-shared-voltage
# Optional: --facts EXTRACT.jsonl.gz --out DIRECTORY
```

This retained output mode uses the same fit as `just fit` and `just forward`, on **all decisive trials**, including record-only facts, and writes only `target-shared-voltage.json`. It requires one nonempty BIOS context and 16-core profiles. The anchor mode rejects `--bootstrap`, `--seed`, `--forward-only` and `--seal`, even when their supplied values equal their defaults. Its command and output file are scheduled for removal only after #480 passes its forward score and regenerates the ensemble.

Before creating the output directory or changing an anchor, it runs the **unchanged model check on the full original extract**, not a bootstrap or selected subset. It reports `ok` only when every eligible group passes (52 groups on the 2026-10-04 extract). A non-ok result prints each exact flagged class, loaded cores, intended duration, depth, observed counts, interval and full-precision mean probability, then exits with failure without writing. A passing in-sample check does not validate later-session predictions.

There is no scalar repair: a failed all-facts check is reported as fitted, never profiled into feasibility. The default ensemble command writes flagged fits under ADR 0044; the retained anchor command refuses to overwrite its output when the unchanged check fails.

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

The generated `notes` array explicitly says **IN-SAMPLE, NOT forward-validated** and records the failed 2026-10-04 [#306](https://github.com/shgew/togi/issues/306#issuecomment-5979625589) bar, the decisive trial count and raw loss, the full-extract model-check statement and the signal-mix failure count; it carries no explanation, which this section owns: the unsupported y-cruncher R7 thresholds above, and the suite-level `replay` setting below. The file declares relative `facts` and `bios_context`. Workloads and cores have stable order; floating-point parameters have full precision. Reports print raw total and per-trial in-sample Bernoulli log loss for all trials and R7, and predicted/observed R7 counts **for record-only purposes**, not as held-out scores. These diagnostics also print for a flagged fit before failure returns without writing an anchor. Fixed inputs reproduce files and reports byte for byte on one host, excluding the elapsed line ([above](#fitting-the-target-machine)).

**Q17 forward bar remains authoritative.** A shared-voltage fit used for forward claims or regeneration of `target-fit-*` must beat the constant predictor's log loss on a held-out session's R7 trials and predict total R7 failures within a factor of 2 ([#105](https://github.com/shgew/togi/issues/105)). The first candidate failed on 2026-10-04: 36.0 predicted against 17 observed (above the allowed 34), despite log loss 0.270 below constant 0.399. That check was not blind; it was not retuned afterwards, and a future forward claim needs a new blind session. This all-facts anchor neither reruns nor supersedes that check and does not authorize `target-fit-*` regeneration.

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

The suite includes the hand-set `shared-voltage`, the `target-shared-voltage` anchor and two R7 adversaries, each with dev seeds 1–12 and holdout seeds 101–112. Both adversaries passed the unchanged model check on all 52 eligible groups of the 2026-10-04 extract. Under ruleset 8, their dev and holdout median `worst_r7_hazard_per_h` exceeded the corresponding unmodified anchor medians by at least 1.5×. `target-r7-vf-boost` changes only AVX2 core 5's required-clock coefficient to 0.06 V/100 MHz; `target-r7-request-gap` changes AVX-512 cores 9–11's required-clock coefficients to 0.06 V/100 MHz and their required-voltage thresholds to 1.18 V, except core 10 at 1.22 V. Request, clock and power parameters stay at the anchor's values. Across measured R7 trials, both machines reproduce the anchor's loaded requests exactly, retaining its residuals against recorded requests. These expose record-only partial failures and untested request-ordered partial loads, not forward-validated hardware failure rates.

Those measurements and the ruleset 8 ratios describe the earlier frozen-copy `target-r7-vf-boost` and `target-r7-request-gap`, written before the anchor's signal mix was regenerated. They are historical and not measurements of today's overlays ([#563](https://github.com/shgew/togi/issues/563)).

### Forward-chained check

After the ensemble's model check, `just fit` orders sessions with decisive trials by their UTC session IDs, comparing equal-second numeric suffixes numerically (`-2` before `-10`). For each session after the first, it fits all earlier sessions' decisive trials once, without bootstrap resampling or model-check constraints, then predicts only the held-out session. These fits stay in memory and do not change the generated machine files.

The prefix fits run in parallel with the same `--jobs` limit; in `just fit` they run alongside the ensemble's fits, and the report prints after the ensemble's. Scoring and pooling remain in session order, so no held-out outcomes enter their own training prefix and the report is unchanged apart from elapsed times.

Each session row names the session ID, ruleset and `training_sessions` count. `trials` and `failures` count held-out decisive trials and observed failures; `predicted` sums the fit's failure probabilities over their intended durations. `log_loss/trial` is the mean Bernoulli log loss, with probabilities clamped to `[1e-4, 1-1e-4]`. It compares the `fit` to a `constant` predictor whose probability, shown in parentheses, is the earlier training trials' failure rate. Lower loss is better: fit loss above the constant means the fit predicts that later session worse than a single average failure rate.

`failures_p<0.01` counts observed failures assigned less than 1% probability: outcomes the model treated as nearly impossible. `exact_matches` is the number and share of held-out trials whose trial class and full profile occur in the earlier training trials. That share bounds what replay could answer from earlier evidence; it does not establish that replay's answers would be correct. `flagged/eligible` counts held-out groups flagged by the unchanged model check, with at least 10 trials and the same 99% binomial intervals. The indented regime rows also show fit and constant log loss over the same regime's trials, using exactly the existing probability clamp and each session's prefix-wide constant.

R7 rows additionally break out each workload (including y-cruncher) and loaded-CCD scope: CCD0 only, CCD1 only, or both CCDs. These scope rows partition the R7 starts; a both-CCD start is not double-counted. Single-core R7 starts also get a separate diagnostic row, while remaining in their workload and CCD totals.

`Pooled` sums all held-out sessions' counts and predictions and divides summed log losses by their total trials. Its constant uses each session's own earlier prefix, not one failure rate fitted to the pooled outcomes; its group counts sum the separate held-out checks. `Forward-chained elapsed` reports the check's own wall time, which in `just fit` overlaps the ensemble's fits. This is evidence about extrapolation to later sessions, not a gate or a guarantee about unseen profiles or other machines.

```sh
just forward            # the check alone: no ensemble, no machine files
just forward --seal 1   # also leave the newest session unfitted and unscored
```

`just forward` runs `togi-tool-fit --forward-only`: the same rows and `Pooled` line, without fitting the ensemble or writing machine files. `--seal N`, accepted only with `--forward-only`, drops the newest N sessions before the check: they are neither fitted nor scored, the remaining rows are unchanged, and `Pooled` covers only the remaining held-out sessions. At least one held-out session must remain. Model research ([program](../tools/fit/program.md)) iterates with the newest session sealed and scores it only to confirm a kept change.

To score the current ensemble beside the forward candidate without refitting its members:

```sh
just forward --reference-machine tools/bench/machines/target-fit-{0,1,2,3,4,5,6,7,8}.json
```

`--reference-machine` accepts one or more files and requires `--forward-only`. Reference scoring uses the same sessions, seal, prefix constants and probability clamp. For each trial it averages the members' failure probabilities before computing log loss, predicted failures and surprises. Its model-check diagnostic is explicitly `flagged_by_any_member/eligible`, not a check of the mean predictor. It is reference evidence, never the candidate's gate.

#### The 2026-10-17 frozen score

The candidate froze at [PR #972](https://github.com/shgew/togi/pull/972)'s merge, `7cfb1f26fc604c1957b91315f05483579891a11d`, at 2026-10-17T08:55:49Z. Neither held-out session had been inspected or extracted before that merge. The frozen fitter ran the unsealed forward check **once**, with the unchanged nine committed members as a non-gating mean-probability reference. [Full raw output](../tools/bench/facts/shared-voltage-score-20261017.txt) is committed and [posted on #480](https://github.com/shgew/togi/issues/480#issuecomment-6232034381).

| Session | Decisive R7 starts | Failures | Candidate predicted | Candidate loss | Constant loss | Reference predicted | Reference loss |
|---|---:|---:|---:|---:|---:|---:|---:|
| 20261004T211726Z | 53 | 0 | 4.4 | 0.1482 | 0.0691 | 0.0 | 0.0004 |
| 20261010T230611Z | 306 | 22 | 90.6 | 0.6676 | 0.2590 | 0.1 | 0.5682 |

**Failed:** both per-session R7 losses exceed the prefix-wide constant; the live prediction is 4.12 times observed, outside the allowed [11, 44]. Workload and CCD rows, including y-cruncher, are in the full output. No ensemble, overlays or anchor were regenerated or deleted; no baseline was re-recorded and no Ruleset 12 gate was registered. The owner's [2026-10-17 decision](https://github.com/shgew/togi/issues/480#issuecomment-6231671066) lets Ruleset 12 ship ungated and moves #490/#896 to Ruleset 13. Nobody retunes against these scored sessions; a later candidate needs unfitted post-freeze sessions. #480 stays open.

The live cut ends at event **15023**, `shutdown`, **2026-10-12T07:27:20.062378965Z**, its last event before the freeze. Trial 0445 ended inconclusive on cancellation and is excluded. The archived session's 54th R7 start, trial 0150, also ended cancelled/inconclusive; the decisive-only extractor therefore scores 53 R7 starts, not the retro's 54 starts.

The refresh contains **3,889 facts** across nine sessions: the original 3,297, 149 from 20261004T211726Z and 443 from 20261010T230611Z. The extraction deviation was recorded on #480 and PR #972 before scoring. The historical Go extractor above produced byte-identical decompressed JSONL for the seven old sessions. Their full session, regime, workload, CCD and pooled forward rows also matched the refreshed extract with `--seal 2`, apart from its seal notice and elapsed times.

- Extract gzip SHA-256: `002112c83e7aaa227fe96532f108952cc9185430b2991e9db21181abe68aa11f`.
- Decompressed JSONL SHA-256: `38f00a16dafdae64cb1457d880d3467b2364c8d13728be10f25da1091fced131`.
- Raw score-output SHA-256: `b674b17d5f46f8a059e4a2e4dacb92ca61ec37fd81cdc069f30bdcd3c7360d69`.

#### Historical development rows

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

### The fit tool (`togi-tool-fit`)

`just fit`, `just fit-shared-voltage` and `just forward` run `togi-tool-fit` with the flags (`--facts`, `--out`, `--seed`, `--bootstrap`, `--jobs`, `--forward-only`, `--seal`, `--reference-machine`, `--shared-voltage-in-sample`) and the objective, search and checks described above. In detail:

- **Threads.** `--jobs N` is the number of threads the whole run uses, fits and candidate scoring alike, and defaults to every CPU. Every candidate of a search step is scored on its own thread into its own slot and the slots are folded in candidate order, so the machine files, the report and the forward-chained rows are byte for byte the same for any `--jobs` and any CPU count; tests run an ensemble with its forward-chained check, and the shared-voltage anchor, on pools of 1, 2 and all CPUs and compare the bytes. The forward-chained check still runs alongside the ensemble.
- **Bootstrap.** Refit `n` resamples with the `rand` crate's `StdRng` seeded `SEED+n`, not Go's PCG. The resampled trials, and with them refits 1 to 8, differ from the committed Go-generated files. The default fit now uses shared voltage rather than the committed ensemble's legacy joint model; it is not expected to reproduce those machine parameters or losses.
- **Files.** Machine files are the simulator crate's own encoding: fields equal to their defaults are left out, `notes` read `Generated by togi-tool-fit` and the group lines list loaded cores as `0,1`. A BIOS context with an empty field is kept as written rather than refilled with a default, so the anchor needs a context but not one with every field set.
- **Shared-voltage objective.** One objective serves every step of every fit: the threshold prior and named-core terms are part of all coordinate and sweep scores. The Go fitter that wrote the committed anchor scored its hazard-shape, flat, idle and shift steps by raw likelihood instead. The committed historical files are not evidence that the new optimizer reaches the same optimum.
- **Report.** The elapsed times are the last lines of the report and are not part of the deterministic output; `mean_p` is printed as `1.234560e-2`.

The committed `tools/bench/machines/target-fit-*.json` and `target-shared-voltage.json` stay the Go tool's output until the ensemble is regenerated deliberately.

### The package

`nix build .#default` builds the `togi` binary with crane; it is `packages.default`, which the module installs. It installs the bash, zsh, fish and nushell completion scripts that `togi completion <shell>` generates and stamps `togi --version` with the flake's revision: the short revision, `<rev>-dirty` for a dirty tree, and `dev` when the flake has neither. Its release dependencies are built once and shared with `togi-tool-sim`, so the `sim-suite-<i>` checks and the package reuse them. `just togi ARGS` runs the binary from the tree, which reports the revision `dev`.

The flake check `package` builds the package with the revision `dev`, requires `togi --version` to print `togi <version.txt>+dev`, and requires `togi completion <shell>` to succeed with output that names `togi` for each of bash, zsh, fish and nushell. It fixes no command list, so the commands later pull requests add do not change it. The `module` check and the VM tests take this build from the check's `package` attribute. Reproduce the check with `just check-one package`.
