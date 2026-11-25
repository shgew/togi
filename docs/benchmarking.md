# Benchmarking

Use `just same` and `just smoke` to [compare versioned mechanics scripts](#proving-unchanged-decisions), or `just audit` to [check a recorded journal](#auditing-journals). These development tools are not shipped and cannot establish a profile's daily-use safety.

The metric bench, fitted models, historical outcome oracle and long simulator suite were retired in [#1559](https://github.com/shgew/togi/issues/1559). The [original bench and its inputs](https://github.com/shgew/togi/tree/archive/pre-cut/tools/bench) remain in source history. The [decision record](decisions.md#rejected) rejects predictive simulation as evidence for tuning policy.

## Methodology

### Fitted machines and target claims

Fitting and model checking were retired by R03 in [#1559](https://github.com/shgew/togi/issues/1559), following the [approved decision](decisions.md#rejected) rejecting predictive simulators and fitted machine models as tuning-policy authorities.

The annotated historical-source tag [`archive/pre-cut`](https://github.com/shgew/togi/tree/archive/pre-cut) retains the [original methodology](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#methodology), fitter and model-check sources. It is not a product release. Reproduction requires that historical source, not current commands.

### Flagged target fits

The archived machines retain their original provenance, constraints and model-check flags. The [historical explanation](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#flagged-target-fits) records their limitations; retirement neither validates a flagged fit nor supplies a target-machine claim.

### Crash cost and stray-crash exclusion

A crash is evidence like another failure, and a cost beside time when choosing tuner rules. Between comparably deep profiles, fewer crashes is better; added crashes need evidence of the depth or safety they buy. The historical Ruleset 11 gate required a median crash count below Ruleset 10's 426.5 on `target-shared-voltage`; that threshold is history, not a current gate.

Exclude stray crashes from this tuner-rule cost: no togi offset was in force, either before any offset was applied in the boot or after the baseline was restored. The tuner cannot act on them; stray-crash streaks remain a recovery concern ([tuner spec](spec/tuner.md)). This exclusion survives the earlier decision to count only reboot time.

Rejected: weighing a crash only by reboot time leaves the goal of fewer crashes unmeasured. That trade-off belongs in rule choice; the retired simulated gate is not a current acceptance criterion. Runtime crash limits, where applicable, are specified in the [tuner spec](spec/tuner-ruleset14.md).

### Day/night gate

Day/night experiment gates, successor registration/scoring and campaign launching were retired by [R01 in #1559](https://github.com/shgew/togi/issues/1559). They are not product acceptance conditions. This removal changes neither production tuning decisions nor `SCHEMA` or `RULESET`.

The annotated historical-source tag [`archive/pre-cut`](https://github.com/shgew/togi/tree/archive/pre-cut) retains the evaluator sources, registrations and [original instructions](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#daynight-gate). It is not a product release. Reproducing those experiments requires that historical source, not current commands.

Ruleset 13's gate was not met: the [reduced development read](https://github.com/shgew/togi/issues/1217#issuecomment-6403197212) was not the registered full score, and holdout was never run. The [B′ research note](research/bprime-research-note.md) and original trial evidence remain historical evidence. Outstanding hardware acceptance is recorded in [#1459](https://github.com/shgew/togi/issues/1459).

### Successor day/night run path

The calendar runner, policy reports and physical-core truth catalog are also retired. Read-only journal replay and the scripted session driver are separate tools and remain available.

#### Successor registration

The [archived registration](https://github.com/shgew/togi/blob/archive/pre-cut/tools/bench/day-night-successor.json) is historical only. It authorizes no campaign, confirmation or deployment.

### Historical gates

The Ruleset 9 gate is defined by [ADR 0038](adr/0038-self-sufficient-cores.md) and [#105](https://github.com/shgew/togi/issues/105); [ADR 0040](adr/0040-located-hunts.md) records the Ruleset 10 gate. The [Ruleset 11 archive](../tools/bench/gates/ruleset-11/) holds its suite, baseline, candidate and scored report. These are historical records, not current-anchor measurements.

The [#493 measurement record](https://github.com/shgew/togi/issues/493#issuecomment-6040930168) identifies the tuner, simulator and regenerated anchor used for its comparison. Earlier measurements of `target-r7-vf-boost` and `target-r7-request-gap` describe frozen copies, not later overlays. See [reproducing an older ruleset](#reproducing-an-older-ruleset) before comparing historical builds.

### Reproducing an older ruleset

**Historical.** Rulesets 9 and 10 ran on the Go build. The paths below (`internal/sim`, `tools/sim`, `internal/simrun`, `.toml` machines) exist only at those revisions, and the recipe runs that revision's own tools in its own dev shell. No current crate reproduces these runs: the current simulator's seeds and streams differ ([Simulating](https://github.com/shgew/togi/blob/archive/pre-cut/docs/simulating.md#the-simulator-togi-sim)).

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

The diff must be empty. In that detached worktree, enter the dev shell and run the [historical one-scenario recipe](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#running-one-scenario), written in that revision's TOML suite and machine format (its machine is the restored `.toml` file). For ruleset 10, use a detached worktree at `645f529` without the port. Both runs must use the same scenario name, seeds, machine and replay setting. Preserve the worktrees and scratch results until analysis ends; their diffs explain the deliberately dirty ruleset-9 records.

For a comparison on a later simulator, review **every** commit that changed the simulator paths since the older ruleset, not just those two signal-mix commits:

```sh
git log --reverse --no-merges --format='%h %s' 84c2a01..origin/main -- \
  internal/sim tools/sim
git log --oneline 645f529..origin/main -- internal/sim tools/sim
```

The two-commit port is not sufficient for main at `2f318490` (2026-10-10). Later changes couple the simulator to `internal/simrun`, session recovery and journal buffering. `0ea69fc1` also changes samples from maps to `machine.PerCore`, with corresponding changes in requests and session code. Copying only `internal/sim` and `tools/sim` then fails to build against the older infrastructure. A current-simulator port must carry those dependencies without importing newer tuning decisions, and still leave the simulator, machine and facts diff empty against its pinned reference. A generic current-main port procedure moves to [the redesign, #493](https://github.com/shgew/togi/issues/493): this needs an infrastructure compatibility boundary, not a guessed cherry-pick list. The pinned recipe above reproduces the documented measurements.

## Auditing journals

`just audit` audits state directories it is given, such as a [copy of a real one](#copying-a-real-state-directory):

```sh
just audit COPY-OF-STATE-DIR OTHER-STATE-DIR   # audit of current and archived journals
```

### The audit (`togi-tool-audit`)

`just audit` runs `togi-tool-audit`:

- Each directory is audited as a run that may still be in progress, so its live journal need not end in a conclusion.
- The report is JSON Lines on stdout, one object per violation with `directory`, `journal` (`live` or an archived session id), `seq`, `check` and `reason`; `check` is a journal check of [Invariants](spec/tuner.md#invariants), or `unreadable` (a directory that cannot be opened or a journal that cannot be read, with neither `journal` nor `seq`). The summary line `directories= journals= violations=` goes to stderr. Exit status 1 means a violation, 2 a usage error, such as no directory.
- The audit takes each directory's writer lock, so a directory a tuner is running is refused. Taking the lock creates the directory's `lock` file and sets its mode to 0711, so audit a copy.
- The audit reads journals leniently ([Journal](spec/journal.md)): an event of a kind this build does not know is left out, a cause that names it is not a bad cause and the gap it leaves is not a sequence violation. Each journal that lost anything is named on stderr (`togi-tool-audit: "DIR" JOURNAL: ...`) before the summary; stdout and the exit status are unchanged.

Before [#546](https://github.com/shgew/togi/issues/546), `just audit` without directories swept every scenario over 200 seeds and flagged wall-time outliers and boot-cap runs. That historical sweep is retired; auditing a real state directory does not assume that its run has ended.

The audit checks SMU intent/write/readback ordering, the offset range ([Invariants](spec/tuner.md#invariants); readbacks and baselines report the hardware and are not bounded), causal references, the conclusion of a finished simulated run. It also checks three Ruleset 13 rules against the journal alone, independently of the tuner: a nonzero `day.applied` target needs an earlier complete `night.qualification` of that exact CCD profile under the firmware in force at that point (CO 0 needs none); a `day.step` promotion may not reach a profile at or deeper than an active `day.veto` (a cleared veto forbids nothing); and a live `exposure.observed` must cite, by cause, an existing `trial.intent`, `trial.end` or `failure`, one of which belongs to the trial it names (an imported exposure cites no event of this session). The per-core decision checks of earlier rulesets (failure-point and combination avoidance, hunt, phase and cycle decisions, the R7 zero-naming rule and the cycles-shutdown count) are retired: those events stay decodable in old journals and are no longer judged. Real journals may still be live, including between a write and its readback. A write followed by a reboot or the end of an archive is an interrupted action, and a same-boot resume's readbacks of every core complete it; archived transitions and reset boundaries are valid session endings. Audit a copy for a fixed view of a live journal.

## Proving unchanged decisions

`just same [base]` compares normalized live and archived journals from both trees over the [same five versioned mechanics scripts](simulating.md#bounded-comparisons). `just smoke [base]` uses only the mapping/stop and uncorrected-MCE/resume cases. Neither compares the earlier research corpus, predicts a target CPU, or exercises the hardware, detection, and backends that the simulator replaces.

The base defaults to `origin/main` and is exported locally without fetching or registering a worktree. The base is the committed export; the head is the current source tree, including local edits. Both trees must declare scripted input contract v1, and both simulator executables must affirm it. A base lacking it is refused before any build or equality verdict. The exact comparison removes only the build version, revision, and corresponding build description from `session.start` and `config.loaded`; ruleset, schema, fixes, and evidence epoch still have to match.

A difference cancels the remaining work and prints the first differing journal path, line number and normalized lines. `--keep-going` counts every difference. Differing run directories are kept, reconstructing them with fresh runs where needed. A child that fails to complete its script, or does not write a journal, is a harness error and never counts as equality.

This comparison is on demand, not a CI gate.

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

## What each check proves

Cite a check as evidence only for the packages it exercises. `just gate` runs local checks, except `package` and cargo-deny's advisory audit, and keeps incremental Cargo Clippy and doctests. `just push-fix` runs local review-fix checks. Both publish check outputs already in the local store to Cachix when a push token is present; CI counts those as passed without a build job. `just check` builds every local-tier flake check. Derivations carrying `passthru.tier = "ci"`, including VM, tool and recovery checks, are skipped by all three commands on every host ([check tiers and safety-evidence ledger](development.md#safety-evidence-ledger)). `just vm [names]` opts into VM builds. PR CI and the release's dispatched full check still select every Linux flake check, including CI-tier checks, and build every output absent from Cachix. CI's VM-specific setup and the opt-in recipe discover VM checks by their `driver` attribute.

`vm-grub` and `vm-runtime` group scenarios into configuration cohorts, run as named subtests one after another in one VM. Moving to the next cohort activates that cohort's complete generation and reboots; scenarios inside a cohort share one boot, and an explicit reset of the units, state files and fake SMU between them stops the later scenario inheriting the earlier one's leftovers. `vm-display` remains a separate single-scenario VM. Every row below keeps the limits of the scenarios it groups.

| Check | Runs | Exercises | Does not prove |
|---|---|---|---|
| `nextest`, `just test`, `just cover <base>` | Nextest on the shipped closure plus simulator and dashboard test support | Product ordinary tests: the tuner, session and journal directly, and trial, detect, smu, hardware and the backends against fakes, fake file trees and fake package trees | Unshipped tool tests, exhaustive recovery campaigns, real systemd scopes, kernel logs or SMU access |
| `nextest-tools`, `just focus <crate>` | CI-tier nextest on unshipped tool crates and their tool-only libraries; `focus` runs any named crate locally | The selected crates' ordinary tests, including scripted CLI and journal-tool tests | Physical hardware behavior |
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
| `just same` | Five named scripts on each tree's `togi-tool-sim`, comparing normalized live and archived journals | The session, tuner and journal behavior at those scripted mapping, stop, recovery, day-application and dead-end seams | Tuning quality, hardware or supported paths not reached by these cases |
| `just smoke` | `mapping-stop` and `mce-resume`, compared as `just same` does | Mapping, signal shutdown and uncorrected-MCE resume behavior on those journals | The other three cases or anything `just same` does not prove |
| `package` | Builds the release `togi` with the revision `dev` and runs it: `togi --version` and `togi completion` for bash, zsh, fish and nushell, as [below](#the-package) | That the package builds and installs, prints `togi <version.txt>+dev` and generates a completion script that names `togi` for each shell | What any command does, the set of commands, or that a completion script loads in its shell |
| `just hardware` | On the Linux target machine as root: `cargo test --features hardware --test hardware` on `togi-smu`, `togi-detect`, `togi-trial` and `togi-hardware`, one binary and one test at a time, every binary even after one fails. The unprivileged `--no-run` form only compiles | The hardware tests: smu's sixteen distinct nonzero offsets written, read back as one vector and returned to verified stock, and its PM table lanes, detect reading the real kernel log, trial's real systemd scopes and workload checks, and hardware's diagnosis leaving offsets unchanged | The host's full preflight, ranking or a tuning session. `vm-runtime` runs trial's scope fixture as root in NixOS without testing real stress programs, the SMU or cpufreq |

## Copying a real state directory

`just copy-state SRC DST` copies `events.jsonl` and every archive entry named after a session (archived journals and the carry, compatibility and `reset --all` markers) into `DST`, creating it; it leaves out `trials/` and everything else. An archive entry so named that is not a regular file, such as a symbolic link, fails the copy naming it before anything is written. Use the copy for journal inspection (`just audit`, `just stats`) or replay.

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

## The comparator (`togi-tool-bench`)

### The `same` command

`just same [base]` and `just smoke [base]` export `base` (default `origin/main`) with `git archive` and run `togi-tool-bench same BASE`. The command also accepts `--head DIR` (otherwise the current directory), `--smoke`, `--semantic [--retired KIND]...`, `--keep-going`, `--no-cache`, `--keep DIR`, `--jobs N`, `--timeout SECONDS`, `--cache DIR`, `--base-sim FILE`, and `--head-sim FILE`. The contract fixes the case inputs and boot bounds; `--suite` and `--max-boots` are not comparison options. Exit 0 means no case differs, 1 means a difference or execution error, and 2 means a usage error. See [semantic normalization](#comparing-the-r04-cap-retirement) for `--semantic`.

The compare command checks both source trees' `crates/togi-tool-sim/scripted-input-version` before building or reading cached runs. It builds `togi-tool-sim` with `cargo build --release --locked` unless that side has an override. Each executable must report the expected version with `scripted-contract`; overrides are checked before either build. The head uses its own `target`; the base uses a locked `same-target` under the cache directory. An interrupt stops waiting or running children and removes their run directories rather than reporting equality.

An exact comparison digests the live `events.jsonl` and every `archive/*.jsonl`, including relative names and sizes. Only build stamps are normalized. A missing or empty journal, failed child, or timeout is not a match. A differing case reports the first differing journal and line and retains both runs. `--keep` retains all runs and implies `--no-cache`.

Cached runs bind the input contract, simulator binary digest and case, and are written only by a run that completed its case. Every selected case is compared, from a cached run or a new one, even when both trees build the same simulator; `--no-cache` forces both sides to execute. Cached journals are rerun when needed to explain a difference. Durations are informational and never affect equality.

## Checking a machine against real evidence

The [archived trial corpus](https://github.com/shgew/togi/blob/archive/pre-cut/tools/bench/facts/target.jsonl.gz) is historical evidence. No current tool regenerates the extract, uses it as an outcome oracle or scores a fitted machine against it. The [historical extraction and model check](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#checking-a-machine-against-real-evidence) and the [2026-10-17 frozen score](#the-2026-10-17-frozen-score) document its provenance.

## Fitting the target machine

`togi-tool-fit` and `togi-tool-modelcheck` are retired. `just fit`, `just fit-artifact` and `just forward-compare` are absent, with no replacement commands. The [approved decision](decisions.md#rejected) rejects fitted forecasts as a way to decide tuning policy.

The [original fitting instructions and implementation](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#fitting-the-target-machine) and [model research program](../tools/fit/program.md) are historical, not executable current gates. The original [machine files](https://github.com/shgew/togi/tree/archive/pre-cut/tools/bench/machines), facts and provenance are preserved in source history.

### In-sample shared-voltage anchor

The archived anchor and overlays are historical artifacts, not forward-validated hardware failure rates. Their [original methodology](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#in-sample-shared-voltage-anchor) and [#563 measurements](https://github.com/shgew/togi/issues/563) remain historical evidence.

### Forward-chained check

Forward fitting and record comparison are retired with no replacement. The [original scoring procedure](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#forward-chained-check) remains historical source; no current command reruns it.

#### The 2026-10-17 frozen score

The [once-only frozen score and extraction provenance on #480](https://github.com/shgew/togi/issues/480#issuecomment-6232034381) and its [raw output](../tools/bench/facts/shared-voltage-score-20261017.txt) are the record, not a recipe to score again. It failed; no ensemble, overlays, anchor or baseline were regenerated and no Ruleset 12 gate was registered. The [owner's decision](https://github.com/shgew/togi/issues/480#issuecomment-6231671066) permits Ruleset 12 to ship ungated and moves #490/#896 to Ruleset 13. Nobody retunes against these spent sessions: a later candidate needs unfitted post-freeze sessions.

The refreshed extract preserves the original research artifact's rows. That is extraction provenance, not validation of the fitted fallback; the [model program](../tools/fit/program.md#the-data) owns the approved research inputs.

### The fit tool (`togi-tool-fit`)

The Rust fitting tool and its artifact and forward-comparison modes are retired. Their [source and original documentation](https://github.com/shgew/togi/tree/archive/pre-cut/crates/togi-tool-fit) remain at `archive/pre-cut`.

### The package

`nix build .#default` builds the `togi` binary with crane; it is `packages.default`, which the module installs. It installs the bash, zsh, fish and nushell completion scripts that `togi completion <shell>` generates and stamps `togi --version` with the flake's revision: the short revision, `<rev>-dirty` for a dirty tree, and `dev` when the flake has neither. Its release dependencies are built once, as the `release-deps` check, which the package reuses. `just togi ARGS` runs the binary from the tree, which reports the revision `dev`.

The flake check `package` builds the package with the revision `dev`, requires `togi --version` to print `togi <version.txt>+dev`, and requires `togi completion <shell>` to succeed with output that names `togi` for each of bash, zsh, fish and nushell. It fixes no command list, so the commands later pull requests add do not change it. The module member of `quality` and the VM tests take this build from the check's `package` attribute. Reproduce the check with `just check-one package`.
