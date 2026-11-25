# Benchmarking

`togi-tool-bench` (`just bench`) runs the legacy session suite and compares simulated session metrics for on-demand research. It is a development program and is not shipped. Its metrics do not establish a profile's daily-use safety.

[Methodology](#methodology) records the retirement of fitted-model evaluation. Day/night scoring and campaign launching are [retired](#daynight-gate); normal bench metrics, baseline, same/smoke, simulator, journal audit, stats and original trial evidence remain.

```sh
just bench [--split dev|holdout|all] [--out FILE] [--baseline FILE] [--keep DIR] [--jobs N] [--max-boots 1000]
```

## Methodology

### Fitted machines and target claims

Fitting and model checking were retired by R03 in [#1559](https://github.com/shgew/togi/issues/1559), following the [approved decision](decisions.md#rejected) rejecting predictive simulators and fitted machine models as tuning-policy authorities. Bench no longer runs or reports calibration checks. Its hazard metrics still describe the selected synthetic model, not measured hardware risk.

The annotated historical-source tag [`archive/pre-cut`](https://github.com/shgew/togi/tree/archive/pre-cut) retains the [original methodology](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#methodology), fitter and model-check sources. It is not a product release. Reproduction requires that historical source, not current commands.

### Flagged target fits

The committed machines retain their original provenance, constraints and model-check flags unchanged. Those flags are historical evidence, not checks performed by today's bench. The [historical explanation](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#flagged-target-fits) records their limitations; retirement neither validates a flagged fit nor supplies a target-machine claim.

### Crash cost and stray-crash exclusion

A crash is evidence like another failure, and a cost beside time when choosing tuner rules. Between comparably deep profiles, fewer crashes is better; added crashes need evidence of the depth or safety they buy. The historical Ruleset 11 gate required a median crash count below Ruleset 10's 426.5 on `target-shared-voltage`; that threshold is history, not a current gate.

Exclude stray crashes from this tuner-rule cost: no togi offset was in force, either before any offset was applied in the boot or after the baseline was restored. The tuner cannot act on them; stray-crash streaks remain a recovery concern ([tuner spec](spec/tuner.md)). This exclusion survives the earlier decision to count only reboot time.

Rejected: weighing a crash only by reboot time leaves the goal of fewer crashes unmeasured and ungated. A runtime crash budget, daily cap or cool-off is not adopted: it gives up depth or idles the machine merely to save reboots. Crash cost enters rule choice and its registered gate, not a new session limit.

## The suite

`tools/bench/suite.json` lists scenarios in a top-level `scenarios` array. Each scenario names a simulator `machine` JSON file in `tools/bench/machines/` (none means the seeded default machine), or an ensemble in `machines`, and two sets of seeds, `dev` and `holdout`. Ensemble seeds select successive members round-robin by their position within each split, not by the seed value. `"replay": true` enables the same-BIOS replay oracle over the selected fitted member:

|Scenario|Models|
|---|---|
|`default`|the seeded default machine|
|`target`|real-fact replay over the all-facts target fit and eight bootstrap refits, all historically flagged by the retired model check and the refits unconstrained ([flagged target fits](#flagged-target-fits)), including CCD0 and CCD1 R7 joints|
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

### Gates

The legacy session suite registers no gate and `just bench` judges none: a session's conclusion cannot measure a profile's daily-use safety. Earlier rulesets' gates are historical records, kept under [`tools/bench/gates/`](../tools/bench/gates/) and not re-scored.

### Day/night gate

Day/night experiment gates, successor registration/scoring and campaign launching were retired by [R01 in #1559](https://github.com/shgew/togi/issues/1559). They are not product acceptance conditions. This removal changes neither production tuning decisions nor `SCHEMA` or `RULESET`.

The annotated historical-source tag [`archive/pre-cut`](https://github.com/shgew/togi/tree/archive/pre-cut) retains the evaluator sources, registrations and [original instructions](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#daynight-gate). It is not a product release. Reproducing those experiments requires that historical source, not current commands.

Ruleset 13's gate was not met: the [reduced development read](https://github.com/shgew/togi/issues/1217#issuecomment-6403197212) was not the registered full score, and holdout was never run. The [B′ research note](research/bprime-research-note.md) and original trial evidence remain historical evidence. Outstanding hardware acceptance is recorded in [#1459](https://github.com/shgew/togi/issues/1459).

### Successor day/night run path

The external run and score commands are retired. The calendar runner, policy reports and physical-core truth libraries remain for their existing callers; they are not a surviving scoring interface.

#### Successor registration

The [archived registration](https://github.com/shgew/togi/blob/archive/pre-cut/tools/bench/day-night-successor.json) is historical only. It authorizes no campaign, confirmation or deployment.

### Historical gates

The Ruleset 9 gate is defined by [ADR 0038](adr/0038-self-sufficient-cores.md) and [#105](https://github.com/shgew/togi/issues/105); [ADR 0040](adr/0040-located-hunts.md) records the Ruleset 10 gate. The [Ruleset 11 archive](../tools/bench/gates/ruleset-11/) holds its suite, baseline, candidate and scored report. These are historical records, not current-anchor measurements. The bench no longer judges any session-suite gate, and the archive is pinned by hash in the bench's tests.

The [#493 measurement record](https://github.com/shgew/togi/issues/493#issuecomment-6040930168) identifies the tuner, simulator and regenerated anchor used for its comparison. Earlier measurements of `target-r7-vf-boost` and `target-r7-request-gap` describe frozen copies, not today's overlays. See [reproducing an older ruleset](#reproducing-an-older-ruleset) before comparing historical builds.

A scenario may also list legacy `smoke` seeds, each one of its dev or holdout seeds. They are no longer inputs to `just smoke`.

A scenario may set `night_hours` (a positive whole number, default 8), the simulated hours after which the operator ends each of its nights, and `night_end` (`"operator"`, the default, `"dead_end"`, or `"operator_or_stock"`, which also passes a stock dead end), how the sim suite expects the night to end ([the session suite](#the-session-suite)). Every scenario of `tools/bench/suite.json` sets `night_hours` to 48, so a night reaches mapping and the crashes a hunt produces. The bench does not read either.

`--suite FILE` runs another scenario file instead, with machine paths relative to it. The [historical adversarial-search protocol](../tools/bench/adversary.md) used this interface; its removed model-check gate is not a current instruction.

The target scenario has 18 dev and 18 holdout seeds: two seeds per ensemble member in each split. Its oracle draws uniformly from decisive facts matching the entire applied profile and trial class (regime, workload, sorted loaded cores and intended duration). Draws are deterministic per seed and trial ID/index. Condition, phase, source ruleset and evidence epoch do not restrict a class match; BIOS context does. The oracle never uses facts from another BIOS context or records without one. Non-matching trials and trial-less failures use the fitted machine. The tuner sees only the resulting journal evidence, not the oracle or its extract.

Every run is a `togi-tool-sim` subprocess with its own state directory, in parallel up to `--jobs`. `--max-boots` bounds each session by simulated work, not wall time: a session still running after that many simulated boots stops with `censored` status ([simulating](simulating.md)), so a slow or loaded host makes a run take longer but never changes its status. `--timeout` (default 30m) is only a safety net against a hung simulator: a run that exceeds it is killed and the command fails with a harness error naming the session, never recording a session outcome. Runs start longest first, by the wall times recorded by earlier runs in the user cache directory (`togi/bench/costs.json` under `$XDG_CACHE_HOME` or `~/.cache` on Linux, `~/Library/Caches` on macOS), and in suite order for sessions without a record. The first failed run cancels the rest. SIGINT and SIGTERM kill the simulators and remove the temporary directories. `--keep DIR` keeps the state directories under a new `DIR/bench-*/<scenario>/<split>-<seed>` (the path is printed), so the read-only commands can inspect a run with `--state-dir`. Without `--keep`, each run's state directory is removed as soon as its metrics are read, so a full-suite run holds at most `--jobs` of them at once.

Each simulator subprocess retains parsed events across its simulated reboots, avoiding journal re-parsing. It still appends every event to `events.jsonl`; `--keep` leaves the journal available for inspection. The parent reads and parses each journal after the child exits, and children are not run in memory or made to report a digest ([temporary-journal policy](development.md#temporary-journals-for-research-children), [#613](https://github.com/shgew/togi/issues/613)). This changes wall-clock overhead, not simulated durations or tuner decisions. Recovery and interruption tests use the file-backed path, and fixed-seed equivalence tests compare journal output byte for byte on the default machine and `target-fit-0.json`.

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

- Each directory is audited as a run that may still be in progress, so its live journal need not end in a conclusion.
- The report is JSON Lines on stdout, one object per violation with `directory`, `journal` (`live` or an archived session id), `seq`, `check` and `reason`; `check` is a journal check of [Invariants](spec/tuner.md#invariants), or `unreadable` (a directory that cannot be opened or a journal that cannot be read, with neither `journal` nor `seq`). The summary line `directories= journals= violations=` goes to stderr. Exit status 1 means a violation, 2 a usage error, such as no directory.
- The audit takes each directory's writer lock, so a directory a tuner is running is refused. Taking the lock creates the directory's `lock` file and sets its mode to 0711, so audit a copy.
- The audit reads journals leniently ([Journal](spec/journal.md)): an event of a kind this build does not know is left out, a cause that names it is not a bad cause and the gap it leaves is not a sequence violation. Each journal that lost anything is named on stderr (`togi-tool-audit: "DIR" JOURNAL: ...`) before the summary; stdout and the exit status are unchanged.

Before [#546](https://github.com/shgew/togi/issues/546), `just audit` without directories swept every scenario over 200 seeds in a bench subprocess of its own and flagged wall-time outliers and boot-cap runs; the bench's own audit replaces the sweep, and a censored run is no longer a violation.

The audit checks SMU intent/write/readback ordering, the offset range ([Invariants](spec/tuner.md#invariants); readbacks and baselines report the hardware and are not bounded), causal references, the conclusion of a finished simulated run. It also checks three Ruleset 13 rules against the journal alone, independently of the tuner: a nonzero `day.applied` target needs an earlier complete `night.qualification` of that exact CCD profile under the firmware in force at that point (CO 0 needs none); a `day.step` promotion may not reach a profile at or deeper than an active `day.veto` (a cleared veto forbids nothing); and a live `exposure.observed` must cite, by cause, an existing `trial.intent`, `trial.end` or `failure`, one of which belongs to the trial it names (an imported exposure cites no event of this session). The per-core decision checks of earlier rulesets (failure-point and combination avoidance, hunt, phase and cycle decisions, the R7 zero-naming rule and the cycles-shutdown count) are retired: those events stay decodable in old journals and are no longer judged. Real journals may still be live, including between a write and its readback. A write followed by a reboot or the end of an archive is an interrupted action, and a same-boot resume's readbacks of every core complete it; archived transitions and reset boundaries are valid session endings. Audit a copy for a fixed view of a live journal.

## Proving unchanged decisions

`just same [base]` compares normalized live and archived journals from both trees over the [same five versioned mechanics scripts](simulating.md#bounded-comparisons). `just smoke [base]` uses only the mapping/stop and uncorrected-MCE/resume cases. Neither compares the earlier research corpus, predicts a target CPU, or exercises the hardware, detection, and backends that the simulator replaces.

The base defaults to `origin/main` and is exported locally without fetching or registering a worktree. Both trees must declare the supported scripted input version, and both simulator executables must affirm it. A base lacking it is refused before any build or equality verdict. The exact comparison removes only the build version, revision, and corresponding build description from `session.start` and `config.loaded`; ruleset, schema, fixes, and evidence epoch still have to match.

A difference cancels the remaining work and prints the first differing journal path, line number and normalized lines. `--keep-going` counts every difference. Differing run directories are kept, reconstructing them with fresh runs where needed. A child that fails to complete its script, or does not write a journal, is a harness error and never counts as equality.

This comparison is on demand, not a CI gate. `just bench` compares research metrics, not exact decisions.

To control concurrency, caching or retention directly, run `togi-tool-bench same` with its own flags, described [below](#the-same-command).

### Comparing the R04 cap retirement

`just same-semantic BASE --retired KIND` compares retained journal meaning with the same fixed inputs as `same`, or the same two-case subset with `--smoke`. It is on demand, not a CI gate or raw-journal equality claim. Historical R04 comparisons predate the scripted input contract; reproducing them requires the comparator of that revision.

The recipe exports the base exactly as `same` does and passes `--semantic` to `togi-tool-bench same`. This mode runs both sides freshly, bypasses digest-cache reads and writes, and compares every selected session. Build artifacts and scheduling costs still reuse the existing harness cache. A failed child or an unreadable journal is an error. `--keep` retains all runs; without it, only differing runs remain.

Normalization is deliberately narrow:

1. Drop only base-side records of explicitly named kinds, accounting for each kind separately. Repeat `--retired KIND` for multiple kinds; there is no implicit retirement. R04 names `ccd.cap`. No head-side event or unlisted kind is removed.
2. On **both sides**, give remaining records consecutive sequence identities, starting at one separately in each journal. Rebase envelope causes and all numeric payload `Seq` references, including `NightId`, nested witnesses, qualification/certificate/ceiling references, and original-source identities whose session journal is present among the live or archived journals. Immutable provenance into external input sessions remains unchanged. Decision/application ordinals, trial IDs, offsets, durations and other numbers are not sequence references.
3. Rebase an envelope **last-event cursor, not evidence**, only when it names immediately preceding retired output: `night.plan` (`night_next::{plan_owed,schedule}`), `night.qualification` in state `start` (`qualify::start_qualification`, the frozen fact-prefix cutoff), and `deadend` for `failure_at_zero` with no core/action (`night_next::dead_end`). This is the complete tuner-source producer table. Move that cursor to the preceding retained event, retain sorted/deduplicated causes, and report its count. Nonadjacent retired causes, causes outside that table, and all payload references to retired records remain errors; unresolved local references also refuse comparison.
4. Retain `same`'s exact build normalization: remove `version`/`rev` and only the recognized build description from `session.start`/`config.loaded`. Schema, ruleset, fixes and evidence epoch remain compared.
5. A random load seed in `trial.start` roles or `trial.signal` is another journal reference: verify it equals that trial's own intent sequence on each side before rebasing it. Later seeds and therefore the actual load phases of simulated and real trials shift when earlier records disappear. For paired signals from the same trial with a verified random schedule and a shifted seed, count changes to `stops`, `conts` and exactly the generated count message separately. Only those count fields and their generated text may vary; the trial, signal kinds, causes, timing, outcomes and failing cores must still match. `togi-sim` counts these toggles from the seed and elapsed trial time; the tuner does not consume the counts.
6. Re-render a differing message only when both original strings exactly match the journal renderer for their own typed payload, and those full payloads already match after rebasing. That covers, for example, the `failure #N` witness in `unit.edge` and `in flight: seq N` in `crash.detected`. The report counts these rendered references per kind; no other wording is ignored.
7. Compare every remaining record as a JSON value in journal order, ignoring JSON key order/spacing only. `day.imported.raw`, which carries a typed payload as JSON text, is parsed, canonically serialized and rebased in its original session's context. No other strings or fields are rewritten or discarded.

For every record class the report shows base/head counts, identical and differing records, message differences and evidence differences. Counts pair the nth record of a class within its journal; full-record equality additionally checks the original cross-class order. Missing records count as differences. A message-only difference **fails equality**, even when that class's evidence is identical; no whole class is silently filtered.

The target, schedule, permission, outcome and replay evidence summaries compare the indicated complete normalized records **excluding only `msg`**:

| Evidence | Record classes |
|---|---|
| Target | `trial.intent`, `trial.start`, `smu.intent`, `day.applied`, `climb.decision` |
| Schedule | `night.plan`, `task.start`, `trial.start` |
| Permission | `ccd.edge`, `unit.edge`, `unit.stop`, `climb.decision`, `night.qualification`, `day.freeze`, `day.veto` |
| Outcome | `trial.end`, `trial.progress`, `failure`, `exposure.observed` |
| Replay | `exposure.observed`, `session.imported`, `successor.imported`, `day.imported`, `trial.carried`, `failure.carried`, `state.rebuilt` |

Each summary reports `identical`, `different` or `absent` with record and differing counts. These are journal-evidence comparisons, not an independent replay execution, a `state.json` comparison, or proof about unexercised paths or physical hardware. Every other record class still participates in the equality verdict.


<a id="the-suite-gate"></a>

## The session suite

The session suite is a research and development tool, not a correctness check ([research policy](development.md#simulations-and-research)). No flake check, `just gate`, `just push-fix`, `just check`, pull request CI or the release's full check runs it; ordinary nextest tests are the correctness checks. Run it on demand when a change to the tuner, the session or the simulator raises a question about how simulated sessions end.

`just sim-suite` runs every session of `tools/bench/suite.json`, dev and holdout alike, on the simulator (`togi-tool-sim`, [Simulating](simulating.md#the-simulator-command-togi-tool-sim)) in one shard with one session per core, then checks its manifest against exactly the run ids the partition gives that shard. A session passes only when it is `clean`:

- its simulated night ended the way its scenario expects: by the simulated operator, who ends every night at the scenario's `night_hours` (default 8; every scenario of `tools/bench/suite.json` sets 48) of simulated time, or, for a scenario with `night_end: "dead_end"`, at a dead end its machine produces, or, for `night_end: "operator_or_stock"` (set for every scenario whose machine has a hazard at stock: a `flat` limit or a shared rail), at a conclusive failure at CO 0 whose cited exposure ran every core at zero; never the 1000-boot cap, and never a dead end where the operator's end was expected;
- its resume state matched a fresh fold of the journal after every crash and at the end;
- the Ruleset 13 fold (`togi_session::ReadState::replay`: the journal fold and the tuner's policy fold) accepts its whole journal;
- `togi-tool-audit` finds no violation in any journal of its state directory.

A scenario that expects a dead end says so with `night_end: "dead_end"` in `suite.json`; no dead end is to be invented to make a session pass. The suite proves that every session concludes cleanly, not that a tuner change concludes sooner: that is `just bench`. [`togi-tool-sim`](simulating.md#the-simulator-command-togi-tool-sim) also has a test that splits one session across processes by `--max-boots` and requires the same journal as an uninterrupted run.

## What each check proves

Cite a check as evidence only for the packages it exercises. `just gate` runs local checks, except `package` and cargo-deny's advisory audit, and keeps incremental Cargo Clippy and doctests. `just push-fix` runs local review-fix checks. Both publish check outputs already in the local store to Cachix when a push token is present; CI counts those as passed without a build job. `just check` builds every local-tier flake check. Derivations carrying `passthru.tier = "ci"`, including VM, tool and recovery checks, are skipped by all three commands on every host ([check tiers and safety-evidence ledger](development.md#safety-evidence-ledger)). `just vm [names]` opts into VM builds. PR CI and the release's dispatched full check still select every Linux flake check, including CI-tier checks, and build every output absent from Cachix. CI's VM-specific setup and the opt-in recipe discover VM checks by their `driver` attribute.

`vm-grub` and `vm-runtime` group scenarios into configuration cohorts, run as named subtests one after another in one VM. Moving to the next cohort activates that cohort's complete generation and reboots; scenarios inside a cohort share one boot, and an explicit reset of the units, state files and fake SMU between them stops the later scenario inheriting the earlier one's leftovers. `vm-display` remains a separate single-scenario VM. Every row below keeps the limits of the scenarios it groups.

| Check | Runs | Exercises | Does not prove |
|---|---|---|---|
| `nextest`, `just test`, `just cover <base>` | Nextest on the shipped closure plus simulator and dashboard test support | Product ordinary tests: the tuner, session and journal directly, and trial, detect, smu, hardware and the backends against fakes, fake file trees and fake package trees | Unshipped tool tests, exhaustive recovery campaigns, real systemd scopes, kernel logs or SMU access |
| `nextest-tools`, `just focus <crate>` | CI-tier nextest on unshipped tool/research crates and their tool-only libraries; `focus` runs any named crate locally | The selected crates' ordinary tests; tool and research tests remain PR and release gates | That a research run completed, or physical hardware behavior |
| `nextest-recovery-1` … `nextest-recovery-8`, `just recovery` | One CI-tier check per paired crash/SMU and kill shard; partition 1 also runs the four warm/cold cases and proves the scheduled test union exactly once | Every durable-event, dual-kind trial-event and SMU cut in the full recovery window, with unchanged recovery, projection and commitment assertions; warm/cold journal and state equality | Physical power-loss behavior, real firmware or kernel behavior |
| `quality` board member, `just board-test` | Nushell assertions with fixed GitHub JSON and time, in `just gate`, `just push-fix` and the flake bundle | Coordination-board readiness, grouping, links, overlaps, interaction expiry, failures and golden text | Live GitHub API availability or authentication, any togi runtime code or hardware |
| `quality` coverage-selection member, `just cover-test` | Nushell assertions over fixed `cargo metadata` JSON, in `just gate`, `just push-fix` and the flake bundle | The package arguments `just cover BASE [CRATE ...]` builds: the product set retained, named workspace packages added once, unknown and non-workspace names refused before any argument is printed | That the product set matches the `nextest` check, that the named tests reach changed lines, any togi runtime code or hardware |
| `quality` SQL member, `just sql-test` | DuckDB over fixed state directories, in `just gate`, `just push-fix` and the flake bundle | The `just sql` views: current and archived journals, plain and gzip samples with the plain file preferred, session-scoped trial joins, a torn tail that does not parse, a directory without archives or samples, reading the views again after a trial's samples were compressed, and refusing a trial directory the user cannot list | That a real or simulated journal's fields still match the views, any togi runtime code or hardware |
| `quality` documentation-link member | `lychee --offline --include-fragments` over every Markdown file, in `just gate`, `just push-fix` and the flake bundle | That each relative link names a file in the repository and each `#fragment` a heading of the file it names | That a web link resolves, or that a link says what its text claims |
| `quality` module member | Evaluates `nix/module.nix` for x86_64-linux on any host (`nix/module-test.nix`), with the package built with the revision `dev` (`package`'s `package`), which the evaluation references without building it | The module's options and generated configuration: GRUB mirror constraints, the grubenv mount dependency, the tuning specialisation's 30 s watchdog and kernel parameters, the restart-limit and leave-tuning-boot scripts, the lock file rule and package overrides | Any code of togi, or that the configuration boots and behaves as evaluated |
| `cargo-deps` | crane's `buildDepsOnly`: the locked dependencies of every workspace crate in the dev profile, the artifact `clippy`, `nextest`, `nextest-tools`, `nextest-recovery-*`, `doc`, the changelog-fragment member of `quality`, `trial-scope-tests` and `vm-recovery-tests` start from | That the locked dependencies build; its own job pushes the artifact to Cachix, since no other check's output references it | Any workspace crate's code or tests |
| `release-deps` | crane's `buildDepsOnly`: the locked dependencies of `togi` in the release profile with link-time optimisation off, the artifact `package` starts from | That those locked dependencies build in the release profile; its own job pushes the artifact to Cachix, since no other check's output references it | Any workspace crate's code or tests |
| `trial-scope-tests` | Linux only: builds `togi-trial`'s `hardware` test binary with the `hardware` feature and installs it as `togi-trial-tests` | That the test binary compiles | Runtime behavior: `vm-runtime` runs the scope fixture |
| `vm-grub` | Linux only: a NixOS VM on GRUB, run as three sequential configuration cohorts, each a full-generation activation and reboot: the module with its tuning specialisation and the package built with the revision `dev`; the module with a stub `togi` that exits 2 for `run` and `restart-limit`; and the module with the shipped `togi` and a tuner that only sleeps (`nix/vm-test.nix`) | **Tuning boot:** the installed `togi`'s dead end at preflight on the tuning boot and the saved entry it clears; a held host lock stopping `togi.service` with exit 3, no restart and the boot, saved entry and state unchanged; the watch and console services; booting the GRUB tuning entry; the armed watchdog's reset when PID 1 freezes. **Restart limit:** the module's units and scripts: three failed starts, the start limit, the fallback leave reason, the return to the normal generation, the cleared GRUB saved entry and the lock file's ownership. **Night:** `togi night` declining and confirming against real GRUB and systemd, the tuning entry selected with the day's selection remembered, a crash reboot staying in the night with the tuner running, and the power button ending a night through the leave hook and selecting the day's entry again exactly. A menu-picked night is simulated: `savedefault` is checked in the generated day and tuning entries, `grub-set-default` selects the tuning entry as a menu pick would, a crash reboot stays in it unmarked, and its power-button end unsets the selection and drops the earlier night's day entry, after which GRUB runs the day entry's `savedefault` on the default boot | Tuning boot: real stress programs, SMU access or offset writes, a passing preflight, or any tuning decision. Restart limit: any code of togi itself, including its own `restart-limit` command. Night: a real key press in the GRUB menu, and the dashboard's F10, which the unit tests pin |
| `vm-runtime` | Linux only: one NixOS VM with four cores, run as two sequential configuration cohorts as named subtests. Only moving between cohorts activates the next complete generation and reboots. In the first, the host-lock scenario and the root trial-scope scenario (`togi-trial-tests`) share an installed module configuration with the shipped package. In the second, a combined configuration with the persistent fake SMU runs the day-observer scenario first (the `togi-day` observer on the normal boot), then, after an explicit reset of the units, state files and fake SMU, the emergency-stop scenario (the production emergency-stop CLI and sleeping helper workloads, which boots the tuning entry). Each scenario keeps the crashes and reboots its own proof needs (`nix/vm-test.nix`) | **Host lock:** NixOS creates a root-only lock at boot; an unrelated user cannot open it or create files in its parent. The installed `togi` refuses a foreign-owned lock and a held lock without changing the inode or contents, tightens a legacy mode to 0600 on the same inode, and `emergency-stop` recreates a deleted lock as root 0600. **Trial scope:** the real-systemd scope tests run as root with helper backends and leave no `togi-*` scope behind; the installed unprivileged `doctor` still skips the scope probe. **Emergency stop:** killing the controller mid-trial, every-core zero readback and removal of real systemd trial scopes before the unchanged 60-second restart; controller affinity during the trial, clean-stop behavior and emergency validation/sweep/readback failures. **Day observer:** the shipped service excluding a second writer while it holds the lock and releasing the same inode on stop; refusal without a session and without creating a journal; a clean stop ending in a final checkpoint of positive active time and the clean stop; a hard reset leaving an end the next boot records as unclean; a `day.users` member answering the pending question without root over `/run/togi/day.sock` while a user outside the group is refused; and the module's group, directory and notifier unit as installed. The observer never writes nonzero offsets to the fake SMU | Host lock: `togi-trial-tests` or the tuning boot. Trial scope: real stress programs or SMU access. Emergency stop: real SMU access, real stress programs or the production session's tuning decisions. Day observer: real SMU access or a real session's tuning decisions |
| `vm-display` | Linux only: a NixOS VM with the tuning specialisation booted into the tuning entry (`nix/vm-test.nix`), as its own single-scenario VM with the plain Linux console | The installed watcher on tty1 as root with `TERM=linux`, its readable preflight screen through `/dev/vcs1`, quit keys leaving the watcher running, Alt F2/F3 reaching login and logs and Alt F1 returning, a killed watcher restarting without changing the tuner or boot, and F10 cancellation followed by confirmation returning to the day | A physical display, real keyboard, real SMU access or a successful tuning session; Rust frame tests pin the palette and rendered layouts |
| `vm-recovery-tests` | Linux only: builds `togi`'s `vm_recovery` integration-test executable as `togi-vm-recovery` | That the production CLI recovery path and its persistent-fake-machine fixture compile | Runtime recovery or real SMU access: `vm-runtime` runs the executable |
| `just sim-suite` | Every suite session on `togi-tool-sim` in one shard, then its manifest check, as [above](#the-session-suite) | The session, tuner, journal and simulator on the paths the suite reaches: each session concludes, resumes warm state equal to a cold fold, replays through the tuner and passes the invariant audit | Better or equal tuning (bench scoring does that), or anything about the packages the simulator replaces |
| `just same` | Five named scripts on each tree's `togi-tool-sim`, comparing normalized live and archived journals | The session, tuner and journal behavior at those scripted mapping, stop, recovery, day-application and dead-end seams | The fitted research corpus, hardware or supported paths not reached by these cases |
| `just smoke` | `mapping-stop` and `mce-resume`, compared as `just same` does | Mapping, signal shutdown and uncorrected-MCE resume behavior on those journals | The other three cases or anything `just same` does not prove |
| `just bench` | Simulated sessions for the chosen split (dev by default), journal audit, summary and cache; with `--baseline FILE`, a comparison with that baseline by metric thresholds | Aggregate research metrics for those fitted sessions | Unchanged decisions (one changed decision with unchanged totals can pass), calibration or hardware reliability |
| `package` | Builds the release `togi` with the revision `dev` and runs it: `togi --version` and `togi completion` for bash, zsh, fish and nushell, as [below](#the-package) | That the package builds and installs, prints `togi <version.txt>+dev` and generates a completion script that names `togi` for each shell | What any command does, the set of commands, or that a completion script loads in its shell |
| `just hardware` | On the Linux target machine as root: `cargo test --features hardware --test hardware` on `togi-smu`, `togi-detect`, `togi-trial` and `togi-hardware`, one binary and one test at a time, every binary even after one fails. The unprivileged `--no-run` form only compiles | The hardware tests: smu's sixteen distinct nonzero offsets written, read back as one vector and returned to verified stock, and its PM table lanes, detect reading the real kernel log, trial's real systemd scopes and workload checks, and hardware's diagnosis leaving offsets unchanged | The host's full preflight, ranking or a tuning session. `vm-runtime` runs trial's scope fixture as root in NixOS without testing real stress programs, the SMU or cpufreq |

## What a run records

`--out FILE` writes one JSON object per run:

- `status`: `concluded` (the simulated operator ended the night and the session stopped cleanly), `deadend`, `censored` or `error`. `censored` is a session still running when `togi-tool-sim` reached its boot cap (`--max-boots`, default 1000; [simulating](simulating.md)): it did not conclude, and its metrics describe the partial session, so its `sim_hours` and `crashes` are lower bounds on its cost;
- `sim_hours`: simulated time from `session.start` to the last event, including the 90 s each crash reboot costs. This is the time to conclusion, or for a run that did not conclude the time it ran;
- `crashes`, `trials` and `trial_hours`;
- `real_answers` and `real_answer_share`: the number and fraction of completed trials answered by matching real facts; inconclusive trials and failures before workload startup count in `trials` but not as real answers;
- `scenario_real_answer_share`: real answers divided by completed trials across the scenario's selected runs, not an average of per-run fractions;
- `machine`: the fitted ensemble member selected for the seed.
- `hazard_per_h` and `hazard_max_per_h`: failures per hour at the session's last tuning profile (its last `profile.change`, or its baseline before one) with every core loaded, per regime, from the simulator's own failure model. A profile that passed by luck shows up here, and no journal can show it.

The commit, a dirty flag and the ruleset are recorded with every run.

The summary counts each scenario's runs, `concluded` runs and `censored` runs. Its time and crash columns include every status, so a censored run's time and crashes enter them as lower bounds.

The summary prints each scenario's `real_answer_share` and a pooled total. Comparison scenario rows and the verdict line print the candidate and baseline shares over paired runs. These fractions describe how much of the observed path has direct real evidence, not a confidence score. Unfinished trials of a censored session have no recorded outcome and are not counted. Hazard metrics describe the fitted fallback, not an empirical oracle hazard; bench emits no calibration report.

For example, `real_answer_share = 0.2` means matching real facts answered one fifth of completed trials, not one fifth of simulated hours or one fifth of the profile's risk. The rest of the path used the fitted fallback or includes completed trials that cannot count as real answers. A low share can simply mean the tuner explored profiles or load classes absent from the extract; it does not measure fit quality. Replay is enabled by the suite, not by the presence of `facts` in a machine file.

## Comparing two versions

Run the base version with `--out base.jsonl` and the candidate with `--baseline base.jsonl`. Runs pair by scenario and seed; a warning counts candidate runs without a base run, and base runs in the candidate's splits without a candidate run. For each pair that both versions concluded, the ratio is candidate `sim_hours` over base `sim_hours`; a pair either side did not conclude, censored included, has no ratio. Each scenario reports the geometric mean of its ratios with a 95% bootstrap interval. The overall ratio weights scenarios equally, and its interval resamples pairs within each scenario. The last line is a verdict:

- `REJECT`: any violation, or the interval's lower bound above 1.0;
- `ACCEPT`: no violation and the interval's upper bound below 1.0;
- `NEUTRAL`: anything else.

Violations:

- **V1:** a run the base concluded no longer concludes, including a censored run.
- **V2:** a run's `hazard_max_per_h` rises by more than 0.01.
- **V3:** `target`, the target machine's replay-oracle ensemble, gets slower overall.

`crash_delta` and `max_hazard_delta` are the mean and the largest of **candidate minus baseline** over paired runs, including runs that did not conclude.

Seeds are deterministic: the same commit always produces the same runs, so rerunning cannot change a result. A change to how the tuner decides moves later sessions onto different random paths, so compare whole scenarios, not single seeds.

Historical legacy session-suite baselines can be recorded with:

```sh
just bench-baseline
```

The historical legacy session-suite baseline lives at `tools/bench/baseline.jsonl` and includes both dev and holdout seeds. It is not the production-policy objective or a reference for day/night scores. R03 leaves this committed evidence unchanged. New baseline output omits only `wall_s` (each run's real duration); every other current record field stays. Comparison does not read `wall_s`, so it prints the same against baseline output as against full `--out` records. Baselines recorded before the oracle ensemble do not cover `target`; do not reuse them for V3.

`just bench-baseline OUT` writes the same projection to another file and leaves the committed one alone:

```sh
just bench-baseline OUT
```

`OUT` defaults to `tools/bench/baseline.jsonl`. The recipe runs `togi-tool-bench --split all --baseline-out OUT`. The flag writes the same run as `--out` with only `wall_s` left out; it may be given together with `--out`, and neither file makes the tree dirty for the `commit` and `dirty` stamps. The projection is `Record::baseline` in the crate, not a filter in the recipe. `togi-tool-bench sanity` and `--baseline` read the committed file in this record format.

Compare legacy session-suite metrics with the historical baseline:

```sh
just bench --split all --baseline tools/bench/baseline.jsonl
```

The historical legacy session-suite baseline is Ruleset 11, recorded by `just bench-baseline` at a clean commit and then projected to the metrics `just bench` still keeps; the Ruleset 10 baseline it replaced is archived with the Ruleset 11 gate.

## Copying a real state directory

`just copy-state SRC DST` copies `events.jsonl` and every archive entry named after a session (archived journals and the carry, compatibility and `reset --all` markers) into `DST`, creating it; it leaves out `trials/` and everything else. An archive entry so named that is not a regular file, such as a symbolic link, fails the copy naming it before anything is written. The copy is the input of `just facts` ([below](#checking-a-machine-against-real-evidence)) and of replaying a session on the simulator.

The state directory is root's, so the copy runs as root, building under root's home, and the copy is then handed back to you. Take the copy while togi is not running a trial: a journal that ends inside a trial resumes, in every replay, as a crash of that trial.

```sh
copy=$(mktemp -d)
trap 'rm -rf "$copy"' EXIT
sudo nix develop --command just copy-state /var/lib/togi "$copy"
sudo chown -R "$(id -u):$(id -g)" "$copy"
```

## Research campaigns

The campaign dispatcher and its launch, resume, evaluator, ledger and report interfaces are retired by [R01 in #1559](https://github.com/shgew/togi/issues/1559). There is no current campaign-launch command or compatibility path.

The [historical campaign instructions](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#research-campaigns), programs and issue briefs remain source history, not authorization to launch a campaign. Individual retained development tools still have the commands documented on this page. Simulations prove mechanics, not tuning policy or hardware reliability ([simulator role](development.md#simulations-and-research)).

## The bench (`togi-tool-bench`)

`just bench` runs `togi-tool-bench`: the runs, metrics, summary, comparison and cache of the bench ([#821](https://github.com/shgew/togi/issues/821)). `just same` and `just smoke` run its `same` command ([#823](https://github.com/shgew/togi/issues/823)), described [below](#the-same-command).

```sh
just bench --split dev --baseline BEFORE.jsonl --out AFTER.jsonl
just bench sanity --records AFTER.jsonl
```

The suite is `tools/bench/suite.json`, read by strict rules (unknown keys refused, a machine file per scenario, a replay oracle for `replay: true`); every scenario's machines and replay oracle are loaded and checked whatever `--split` selects. A run builds `togi-tool-sim` from this tree with `cargo build --release --locked`, copies the binary into a private directory, and starts that copy as a subprocess for every run with a temporary `--state-dir` whose journal the bench reads after it exits ([temporary-journal policy](development.md#temporary-journals-for-research-children)), in a process group of its own, with the flags `just sim` takes; `--sim PATH` copies and runs a given binary instead. `--jobs` (default: the CPU count) bounds the simulators at once, `--timeout SECONDS` is the hang safety limit of one run (an overrun is a harness error, not a run outcome), and `--max-boots` (default 1000) is the boot cap that makes a run censored. A harness error stops the simulators still running and is what the bench reports. A session a signal killed or stopped is an `error` record and is not cached. `--keep DIR` retains every run directory and skips the cache. The records come out in suite order whatever `--jobs` is, so they do not depend on scheduling. The bench records the tree's commit and dirty flag, so it runs inside a Git work tree. Ctrl-C stops the simulators that are running, discards kept run directories and exits non-zero.

After a run's record is read and before its state directory is removed, the bench audits every journal of the directory, archives included, with [the audit](#the-audit-togi-tool-audit)'s library ([#546](https://github.com/shgew/togi/issues/546)). A `concluded` or `deadend` run is audited as finished, so its live journal must end in its conclusion; a `censored` or `error` run is audited as an unfinished prefix. A journal the lenient read left anything out of is a warning. The report prints `audit: audited=N violations=M`, then each violation as `audit: RUN [JOURNAL] [seq N] CHECK: reason`; a run with a violation is not cached, and the bench exits 1 with `the journal audit found M violations` after writing its records and report. A cached record was audited when it was made. The `same` command audits nothing: it compares journals byte for byte.

The record is JSON Lines: every field is required except `machine` and `wall_s`, which are left out when absent. Baseline output omits only `wall_s`; comparison does not read it. Unknown keys, including the retired `model_check` field, are refused. `hazard_per_h` is an object by regime, and `evidence` and `ruleset` are the build's `togi_tuner::EVIDENCE_EPOCH` and `RULESET`. A run's real answers come from the same extraction as the facts extract and count only decisive trials whose `trial.start` was journaled. The comparison, summary and verdict rules (V1–V3, the equal-scenario-weight bootstrap seeded with 1, 10000 resamples) are those of [Comparing two versions](#comparing-two-versions), except that the warning about unmatched runs counts the candidate runs without a baseline and the baseline runs in the candidate's splits without a candidate directly, so it never prints a negative count. A record file with a run twice, or of the earlier Go format, is refused: `--baseline` takes records of this format only.

The cache holds finished records under `$XDG_CACHE_HOME/togi/bench` (or `~/.cache/togi/bench` on Linux and `~/Library/Caches/togi/bench` on macOS; `--cache DIR` and `--no-cache` override), a directory per tree key and the eight most recently used kept. The tree key binds the SHA-256 of the simulator copy that runs, the bench's own executable's SHA-256 and `--max-boots`; an entry also binds the suite file's bytes, the run's scenario, split and seed, the replay flag, and the bytes of the machine file, every file it extends and its facts extract. Any change misses. A failed cache write is a warning; the record is kept. A cache hit restamps the commit and dirty flag, so a cached record is the one a fresh run would write.

`togi-tool-bench sanity --records FILE` compares a run of the whole suite with the scores in the recorded `tools/bench/baseline.jsonl`. A change to the tuner or simulator can move any seed's outcome, so what must agree is each metric's mean over a scenario's seeds. A line is flagged when the means differ by more than the metric's absolute floor and its relative band (5% for hours and trials) and by more than 3 standard errors (Welch within a scenario; for the total, stratified by scenario), so only a difference the seeds' spread does not explain counts. Any drop in the share of concluded runs is flagged by its size alone, and so is a metric that one side measured and the other did not, or measured on a different number of runs; the table prints both run counts. The bands are in `crates/togi-tool-bench/src/sanity.rs`. It exits 1 when any line is flagged: a flag is a question to answer, not a gate. With 8 runs a side, 3 standard errors is a two-sided p between about 0.01 and 0.02 under Student's t, and the table has about 150 lines, so a flag in a small scenario is expected now and then by chance; answer it with fresh seeds of that scenario on the candidate and on the commit that recorded the baseline.

### The `same` command

`just same [base]` and `just smoke [base]` export `base` (default `origin/main`) with `git archive` and run `togi-tool-bench same BASE`. The command also accepts `--head DIR` (otherwise the current directory), `--smoke`, `--semantic [--retired KIND]...`, `--keep-going`, `--no-cache`, `--keep DIR`, `--jobs N`, `--timeout SECONDS`, `--cache DIR`, `--base-sim FILE`, and `--head-sim FILE`. The contract fixes the case inputs and boot bounds; `--suite` and `--max-boots` are not comparison options. Exit 0 means no case differs, 1 means a difference or execution error, and 2 means a usage error. See [semantic normalization](#comparing-the-r04-cap-retirement) for `--semantic`.

The compare command checks both source trees' `crates/togi-tool-sim/scripted-input-version` before building or reading cached runs. It builds `togi-tool-sim` with `cargo build --release --locked` unless that side has an override. Each executable must report the expected version with `scripted-contract`; overrides are checked before either build. The head uses its own `target`; the base uses a locked `same-target` under the cache directory. An interrupt stops waiting or running children and removes their run directories rather than reporting equality.

An exact comparison digests the live `events.jsonl` and every `archive/*.jsonl`, including relative names and sizes. Only build stamps are normalized. A missing or empty journal, failed child, or timeout is not a match. A differing case reports the first differing journal and line and retains both runs. `--keep` retains all runs and implies `--no-cache`.

Cached runs bind the input contract, simulator binary digest and case, and are written only by a run that completed its case. Every selected case is compared, from a cached run or a new one, even when both trees build the same simulator; `--no-cache` forces both sides to execute. Cached journals are rerun when needed to explain a difference. Durations are informational and never affect equality.

## Checking a machine against real evidence

A machine file may declare `"facts": "../facts/target.jsonl.gz"`, resolved relative to the machine JSON file. Every `target-fit-*.json` declares the committed target-machine extract for replay. Bench no longer checks machines against these facts; the [historical check](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#checking-a-machine-against-real-evidence) is retired. Fact extraction and replay remain available.

Regenerate the extract from a temporary copy of the state directory, never the live state: `togi-tool-facts` takes the directory's writer lock. Copy it as [above](#copying-a-real-state-directory), while togi is not running a trial:

```sh
copy=$(mktemp -d)
trap 'rm -rf "$copy"' EXIT
sudo nix develop --command just copy-state /var/lib/togi "$copy"
sudo chown -R "$(id -u):$(id -g)" "$copy"
just facts "$copy"
```

`togi-tool-facts` reads every archived and live session through `togi-tool-trialfacts` and the `togi-facts` extraction, including sessions before `reset --all` and older builds. Its gzip JSON Lines output is byte-stable for fixed inputs. Generation rejects sources with no facts and writes to a temporary file beside the destination; the committed extract is replaced only after extraction and file close succeed, so an unreadable or empty source leaves it unchanged. Records retain session, build, ruleset, BIOS context (the one in force when the trial opened or the idle failure was recorded), sequence, trial ID, kind, class, condition, phase, record-only status, full profile, outcome, signal, measured duration and, when present, `voltage_requests_v`, `top_requesters`, `ccd_mhz` and `stalled_core`. These are machine measurements, not personal data. Native generation uses only journal fields; request and clock summaries for older trial ends come from the 2026-10-17 Go-era refresh described below, not native sample extraction. Records omit journal timestamps, paths, hostnames and boot IDs. `togi-tool-trialfacts` is the shared reader for evaluation tools.
The extract also retains an optional attributed `core` on failures, including core 0, so a named computation error constrains the core it actually names rather than the first loaded core. Older records omit it.

The native reader does not import Go-era journal headers. The committed 2026-10-17 refresh used the unmodified Go extractor at `ec7452cbd37fc826fab96fe4d0b877e040668c94`, with persisted per-trial samples to recover request and clock summaries. Its provenance and once-only score are [recorded below](#the-2026-10-17-frozen-score); this is not a native journal compatibility promise.

## Fitting the target machine

`togi-tool-fit` and `togi-tool-modelcheck` are retired. `just fit`, `just fit-artifact` and `just forward-compare` are absent, with no replacement commands. The [approved decision](decisions.md#rejected) rejects fitted forecasts as a way to decide tuning policy.

The [original fitting instructions and implementation](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#fitting-the-target-machine) remain under the annotated historical-source tag `archive/pre-cut`. The retained [model research program](../tools/fit/program.md) is historical, not an executable current gate. Machine files, facts, provenance and original evidence remain unchanged.

### In-sample shared-voltage anchor

The anchor-generation mode is retired with the fitter. The committed anchor and overlays remain historical simulator inputs, not forward-validated hardware failure rates. Their [original methodology](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#in-sample-shared-voltage-anchor) and [#563 measurements](https://github.com/shgew/togi/issues/563) remain historical evidence.

### Forward-chained check

Forward fitting and record comparison are retired with no replacement. The [original scoring procedure](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#forward-chained-check) remains historical source; no current command reruns it.

#### The 2026-10-17 frozen score

The [once-only frozen score and extraction provenance on #480](https://github.com/shgew/togi/issues/480#issuecomment-6232034381) and its [raw output](../tools/bench/facts/shared-voltage-score-20261017.txt) are the record, not a recipe to score again. It failed; no ensemble, overlays, anchor or baseline were regenerated and no Ruleset 12 gate was registered. The [owner's decision](https://github.com/shgew/togi/issues/480#issuecomment-6231671066) permits Ruleset 12 to ship ungated and moves #490/#896 to Ruleset 13. Nobody retunes against these spent sessions: a later candidate needs unfitted post-freeze sessions.

The refreshed extract preserves the original research artifact's rows. That is extraction provenance, not validation of the fitted fallback; the [model program](../tools/fit/program.md#the-data) owns the approved research inputs.

### The fit tool (`togi-tool-fit`)

The Rust fitting tool and its artifact and forward-comparison modes are retired. Their [source and original documentation](https://github.com/shgew/togi/tree/archive/pre-cut/crates/togi-tool-fit) remain at `archive/pre-cut`; the committed `tools/bench/machines/target-fit-*.json` and `target-shared-voltage.json` remain unchanged historical outputs.

### The package

`nix build .#default` builds the `togi` binary with crane; it is `packages.default`, which the module installs. It installs the bash, zsh, fish and nushell completion scripts that `togi completion <shell>` generates and stamps `togi --version` with the flake's revision: the short revision, `<rev>-dirty` for a dirty tree, and `dev` when the flake has neither. Its release dependencies are built once, as the `release-deps` check, which the package reuses. `just togi ARGS` runs the binary from the tree, which reports the revision `dev`.

The flake check `package` builds the package with the revision `dev`, requires `togi --version` to print `togi <version.txt>+dev`, and requires `togi completion <shell>` to succeed with output that names `togi` for each of bash, zsh, fish and nushell. It fixes no command list, so the commands later pull requests add do not change it. The module member of `quality` and the VM tests take this build from the check's `package` attribute. Reproduce the check with `just check-one package`.
