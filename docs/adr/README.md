# Architecture decisions

The specs describe current behavior; ADRs record why and the alternatives rejected. Where they disagree, the spec wins and this index is out of date.

ADRs cover product, architecture and safety decisions. Development-process choices (checks, CI, caching, releases, review, tooling) live in [Development](../development.md), [REVIEW.md](../../REVIEW.md), [the review record](../review-record.md) or [the review command](../../.omp/commands/review-pr.md), each with a short rationale, and change by editing that document. The process ADRs below remain as history.

A new ADR adds its line here and updates the status of any ADR it supersedes. No ADR file is deleted or moved: a historical ADR keeps its text, and its line names what replaced it. ADRs 0001–0034 keep their historical vocabulary; [0034](0034-one-vocabulary-from-screen-to-journal.md) and [0037](0037-cycles-and-trials.md) translate it to `GLOSSARY.md`, and [ADR 0020's terms](0020-hunt-and-refine.md#terms-today) map its pre-Ruleset-13 rules. [Ruleset history](../how-togi-tunes.md#ruleset-history) owns the chronology of ruleset changes.

## Current

- [0002](0002-clean-room-rewrite.md) Clean-room rewrite, not a port of linux-corecycler — in force.
- [0003](0003-journal-is-source-of-truth.md) The journal is the source of truth; no database — amended by [0077](0077-verified-folded-state-snapshots.md).
- [0005](0005-grub-tuning-boot.md) Unattended tuning is a GRUB boot entry — in force.
- [0008](0008-self-checking-workloads.md) Every workload checks its own results — in force.
- [0009](0009-compatibility-across-updates.md) Compatibility across updates — partly superseded by [0019](0019-a-ruleset-change-starts-a-seeded-session.md) (refusal of older journals).
- [0010](0010-development-programs-in-tools.md) Development programs live in tools/ — in force; the programs are now the `togi-tool-*` crates ([#918](https://github.com/shgew/togi/issues/918)).
- [0013](0013-candidate-edges-for-a-new-session.md) Candidate edges start a new session in confirmation — partly superseded by [0019](0019-a-ruleset-change-starts-a-seeded-session.md), [0020](0020-hunt-and-refine.md) and [0027](0027-carry-trial-facts.md); configured candidate solo limits remain.
- [0014](0014-release-from-a-workflow.md) Releases are made by a workflow started by hand — partly superseded by [0016](0016-every-pull-request-runs-every-check.md) (release checks) and [0033](0033-changelog-fragments.md) (entries).
- [0017](0017-publish-as-togi-from-a-fresh-repository.md) Publish as togi from a fresh repository — in force.
- [0019](0019-a-ruleset-change-starts-a-seeded-session.md) A ruleset change starts a seeded session — partly superseded by [0084](0084-a-fresh-ruleset-transition.md) (Ruleset 13), earlier by [0020](0020-hunt-and-refine.md) and [0027](0027-carry-trial-facts.md).
- [0020](0020-hunt-and-refine.md) Hunt the core behind every failure and refine every core to its mark — partly superseded by [0079](0079-day-observer-night-tester.md) and [0080](0080-per-ccd-first.md) (Ruleset 13), earlier by 0023, 0024, 0028, 0038, 0054 and 0070.
- [0021](0021-cache-check-outputs-on-cachix.md) Cache check outputs on Cachix — amended by [0042](0042-opt-in-to-the-check-cache-locally.md), [0045](0045-the-tight-loop-reuses-cached-test-results.md) and [0085](0085-the-gate-publishes-and-pull-request-ci-runs-every-check.md).
- [0024](0024-schedule-from-uncontradicted-evidence.md) Schedule from uncontradicted evidence — partly superseded by [0079](0079-day-observer-night-tester.md) (Ruleset 13), earlier by 0027, 0028 and 0054.
- [0025](0025-recorded-agent-review.md) Recorded agent review — partly superseded by [0026](0026-review-only-what-changed.md), [0031](0031-pull-requests-under-the-owners-account.md), [0039](0039-remind-only-the-main-session.md) and [0041](0041-coordinator-reviews-small-pull-requests.md).
- [0026](0026-review-only-what-changed.md) Review only what changed — partly superseded by [0030](0030-coverage-is-a-review-signal.md) (record format).
- [0027](0027-carry-trial-facts.md) Carry trial facts across ruleset changes — partly superseded by [0084](0084-a-fresh-ruleset-transition.md) (Ruleset 13), earlier by 0038.
- [0028](0028-remove-tiers.md) Report qualified rotations instead of durability tiers — partly superseded by [0054](0054-two-phases-replace-the-deepening-loop.md); its opaque-event clause gave way to [#877](https://github.com/shgew/togi/issues/877).
- [0029](0029-bench-verdicts-rest-on-fitted-machines.md) Bench verdicts rest on fitted machines — amended by [0044](0044-write-flagged-target-fits.md).
- [0030](0030-coverage-is-a-review-signal.md) Coverage is a review signal, not a target — partly superseded by [0032](0032-coverage-lists-uncovered-changed-lines.md).
- [0031](0031-pull-requests-under-the-owners-account.md) Pull requests under the owner's account — in force.
- [0032](0032-coverage-lists-uncovered-changed-lines.md) Coverage lists uncovered changed lines — in force.
- [0033](0033-changelog-fragments.md) Changelog entries are per-pull-request fragments — in force.
- [0034](0034-one-vocabulary-from-screen-to-journal.md) One vocabulary from screen to journal — in force; [0037](0037-cycles-and-trials.md) renames the checking schedule and pass-evidence unit.
- [0035](0035-gpl-3.0-or-later.md) License togi under GPL-3.0-or-later — in force.
- [0036](0036-flake-checks-stay-hermetic.md) Flake checks stay hermetic — in force.
- [0037](0037-cycles-and-trials.md) Cycles and trials — in force.
- [0039](0039-remind-only-the-main-session.md) Remind only the main session — in force.
- [0041](0041-coordinator-reviews-small-pull-requests.md) The coordinator reviews small pull requests — amended by [0051](0051-light-checks-for-review-fixes.md).
- [0042](0042-opt-in-to-the-check-cache-locally.md) Opt in to the check cache locally — in force.
- [0043](0043-record-work-that-waits-on-the-target-machine.md) Record work that waits on the target machine — in force.
- [0044](0044-write-flagged-target-fits.md) Write flagged target fits — in force.
- [0047](0047-gate-before-every-push.md) Gate before every push — amended by [0051](0051-light-checks-for-review-fixes.md), [0061](0061-no-macos-ci.md), [0074](0074-pull-request-ci-gates-landing-on-main.md), [0075](0075-nextest-runs-in-dependency-layer-groups.md), [0085](0085-the-gate-publishes-and-pull-request-ci-runs-every-check.md), [0087](0087-simulations-are-research-tools-not-checks.md), [0089](0089-heavy-checks-run-on-ci-runners-not-on-the-contributors-machine.md) and [0090](0090-nextest-runs-by-concern.md).
- [0048](0048-rust.md) Rust as the implementation language — partly superseded by [0058](0058-native-rust-rewrite.md) (compatibility contract) and [0071](0071-no-go-in-the-repository.md) (tool languages).
- [0050](0050-json-for-first-party-data-files.md) JSON for every first-party data file — in force.
- [0051](0051-light-checks-for-review-fixes.md) Light checks for review fixes — amended by [0061](0061-no-macos-ci.md), [0074](0074-pull-request-ci-gates-landing-on-main.md), [0075](0075-nextest-runs-in-dependency-layer-groups.md), [0085](0085-the-gate-publishes-and-pull-request-ci-runs-every-check.md), [0087](0087-simulations-are-research-tools-not-checks.md), [0089](0089-heavy-checks-run-on-ci-runners-not-on-the-contributors-machine.md) and [0090](0090-nextest-runs-by-concern.md).
- [0052](0052-crashes-are-a-cost.md) Crashes are a cost — in force.
- [0057](0057-running-the-rust-migration-unattended.md) Running the Rust migration unattended — partly superseded by [0058](0058-native-rust-rewrite.md), [0061](0061-no-macos-ci.md) and [0072](0072-agents-share-a-host-without-locks.md); its parking and stop list remain.
- [0058](0058-native-rust-rewrite.md) A native Rust rewrite, not a byte-compatible port — in force; [0061](0061-no-macos-ci.md) supersedes its macOS choice.
- [0059](0059-the-tuner-folds-and-never-queries-history.md) The tuner folds the journal and never queries its history — in force.
- [0060](0060-bench-children-keep-temporary-journals.md) Bench children keep temporary journals — in force.
- [0061](0061-no-macos-ci.md) No CI job runs on macOS — in force; [0074](0074-pull-request-ci-gates-landing-on-main.md) supersedes its `main`-run cancellation.
- [0062](0062-checks-need-no-race-detector-or-test-result-cache.md) Checks need no race detector or test-result cache — partly superseded by [0063](0063-only-shared-hardware-tests-are-serialized.md) (process-test cap).
- [0063](0063-only-shared-hardware-tests-are-serialized.md) Only shared-hardware tests are serialized — in force.
- [0071](0071-no-go-in-the-repository.md) No Go in the repository — in force.
- [0073](0073-tests-and-simulations-run-in-memory.md) Tests and simulations run in memory — in force.
- [0074](0074-pull-request-ci-gates-landing-on-main.md) Pull request CI gates landing on main — partly superseded by [0085](0085-the-gate-publishes-and-pull-request-ci-runs-every-check.md) and [0087](0087-simulations-are-research-tools-not-checks.md); amended by [0075](0075-nextest-runs-in-dependency-layer-groups.md), [0089](0089-heavy-checks-run-on-ci-runners-not-on-the-contributors-machine.md) and [0090](0090-nextest-runs-by-concern.md).
- [0076](0076-kmscon-tuning-display.md) kmscon is the optional tuning-boot display — in force.
- [0077](0077-verified-folded-state-snapshots.md) Verified folded-state snapshots are a cache, never authority — in force.
- [0078](0078-controller-clock-and-rotation.md) Cap isolated controller clocks and rotate loaded hosts — in force.
- [0079](0079-day-observer-night-tester.md) Days observe; nights test — in force.
- [0080](0080-per-ccd-first.md) Tune one offset per CCD first — in force.
- [0081](0081-margin-from-measured-slope.md) Set the cap from measured slope — in force.
- [0082](0082-bios-stays-at-zero.md) Keep BIOS Curve Optimizer at zero — in force.
- [0083](0083-ask-after-unclean-ends.md) Ask after unclean daily ends — in force.
- [0084](0084-a-fresh-ruleset-transition.md) Start Ruleset 13 fresh, retaining raw daily observations — in force.
- [0085](0085-the-gate-publishes-and-pull-request-ci-runs-every-check.md) The gate publishes and pull request CI runs every check — partly superseded by [0087](0087-simulations-are-research-tools-not-checks.md); amended by [0089](0089-heavy-checks-run-on-ci-runners-not-on-the-contributors-machine.md) and [#1429](https://github.com/shgew/togi/issues/1429) (`quality` packing).
- [0087](0087-simulations-are-research-tools-not-checks.md) Simulations are research tools, not checks — in force.
- [0089](0089-heavy-checks-run-on-ci-runners-not-on-the-contributors-machine.md) Heavy checks run on CI runners, not on the contributor's machine — in force; [#1429](https://github.com/shgew/togi/issues/1429) packs `quality`.
- [0090](0090-nextest-runs-by-concern.md) Nextest runs by concern — in force.
- [0091](0091-tests-cost-what-they-prove-and-merge-units-are-planned.md) Tests cost what they prove, and merge units are planned before code — in force.

## Historical

- [0001](0001-go.md) Go as the implementation language — superseded by [0048](0048-rust.md) and [0071](0071-no-go-in-the-repository.md).
- [0004](0004-find-only.md) togi finds offsets; BIOS applies them — superseded by [0082](0082-bios-stays-at-zero.md) (Ruleset 13).
- [0006](0006-start-from-bios-values.md) Search starts from the BIOS values — superseded by [0082](0082-bios-stays-at-zero.md) (Ruleset 13).
- [0007](0007-suspect-backoffs.md) Unattributed failures back off as suspects; depth is regained only on request — superseded by [0011](0011-blame-by-load-and-automatic-regain.md).
- [0011](0011-blame-by-load-and-automatic-regain.md) Blame unattributed failures by load and regain depth automatically — superseded by [0015](0015-avx-512-first-in-confirmation.md), [0020](0020-hunt-and-refine.md) and [0038](0038-self-sufficient-cores.md).
- [0012](0012-release-by-push.md) Releases open with a push and publish from CI — superseded by [0014](0014-release-from-a-workflow.md).
- [0015](0015-avx-512-first-in-confirmation.md) Run mprime AVX-512 first in confirmation — superseded by [0020](0020-hunt-and-refine.md).
- [0016](0016-every-pull-request-runs-every-check.md) Every pull request runs every flake check; the release trusts `main` — superseded by [0074](0074-pull-request-ci-gates-landing-on-main.md) and [0087](0087-simulations-are-research-tools-not-checks.md).
- [0018](0018-crashes-are-not-a-cost.md) Crashes are not a cost — superseded by [0052](0052-crashes-are-a-cost.md); its exclusion of stray crashes remains.
- [0022](0022-run-the-race-detector-in-ci.md) Run the race detector in CI — superseded by [0062](0062-checks-need-no-race-detector-or-test-result-cache.md).
- [0023](0023-hunts-that-converge-on-shared-voltage.md) Hunts that converge on shared voltage — superseded by [0080](0080-per-ccd-first.md) (Ruleset 13), earlier by 0038.
- [0038](0038-self-sufficient-cores.md) Self-sufficient cores under shared voltage — superseded by [0080](0080-per-ccd-first.md) and [0081](0081-margin-from-measured-slope.md) (Ruleset 13); honest partial-load generation remains.
- [0040](0040-located-hunts.md) Locate multi-core R7 failures before charging the loaded cores — superseded by [0080](0080-per-ccd-first.md) (Ruleset 13), earlier in part by 0053.
- [0045](0045-the-tight-loop-reuses-cached-test-results.md) The tight loop reuses cached test results — superseded by [0062](0062-checks-need-no-race-detector-or-test-result-cache.md).
- [0046](0046-macos-checks-the-package-on-main.md) macOS checks only the package, on `main` — superseded by [0061](0061-no-macos-ci.md).
- [0049](0049-count-self-sufficiency-per-core-and-workload.md) Count self-sufficiency per core and workload — superseded by [0080](0080-per-ccd-first.md) and [0081](0081-margin-from-measured-slope.md) (Ruleset 13).
- [0053](0053-escalation-only-located-hunts.md) Locate multi-core R7 failures only when backing off loaded cores stops helping — superseded by [0080](0080-per-ccd-first.md) (Ruleset 13).
- [0054](0054-two-phases-replace-the-deepening-loop.md) Two phases replace the deepening loop — superseded by [0079](0079-day-observer-night-tester.md) (Ruleset 13).
- [0055](0055-rederive-r7-chains-after-a-backoff.md) Re-derive R7 chains after a backoff — superseded by [0079](0079-day-observer-night-tester.md) (Ruleset 13).
- [0056](0056-later-failures-step-back-in-pairs.md) Later failures step a core back in pairs — superseded by [0079](0079-day-observer-night-tester.md) (Ruleset 13).
- [0064](0064-locate-when-passes-clear-the-loaded-cores.md) Locate a multi-core R7 failure at once when passes clear its loaded cores — superseded by [0080](0080-per-ccd-first.md) and [0084](0084-a-fresh-ruleset-transition.md) (Ruleset 13).
- [0065](0065-test-repeated-r7-charges.md) Test repeated R7 charges before rerunning the full load — superseded by [0080](0080-per-ccd-first.md) (Ruleset 13).
- [0066](0066-short-crashes-take-no-voltage-target.md) Short crashes take no voltage target — superseded by [0080](0080-per-ccd-first.md) (Ruleset 13).
- [0067](0067-named-targets-stay-on-their-ccd.md) Named targets stay on their CCD — superseded by [0080](0080-per-ccd-first.md) (Ruleset 13).
- [0068](0068-cap-named-non-crash-r7-backoffs.md) Cap named non-crash R7 backoffs — superseded by [0080](0080-per-ccd-first.md) (Ruleset 13).
- [0069](0069-yield-without-finishing-phase-two-movers.md) Yield without finishing phase-two movers — superseded by [0079](0079-day-observer-night-tester.md) (Ruleset 13).
- [0070](0070-probe-reproduced-singleton-failures.md) Probe reproduced singleton failures — superseded by [0080](0080-per-ccd-first.md) (Ruleset 13).
- [0072](0072-agents-share-a-host-without-locks.md) Agents share a host without locks — historical: host scheduling is outside the repository.
- [0075](0075-nextest-runs-in-dependency-layer-groups.md) Nextest runs in dependency-layer groups — superseded by [0090](0090-nextest-runs-by-concern.md); narrowed source and fixture selection remain.
- [0086](0086-heavy-runs-wait-for-memory.md) Heavy runs wait for memory, not for each other — historical: host scheduling is outside the repository.
- [0088](0088-the-repository-bounds-its-own-build-and-test-parallelism.md) The repository bounds its own build and test parallelism — withdrawn: host scheduling is outside the repository.
