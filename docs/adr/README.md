# Architecture decisions

The specs describe current behavior; ADRs record why and the alternatives rejected; where they disagree, the spec wins and this index is out of date.

A new ADR adds its line here and updates the status of any ADR it supersedes.

ADRs 0001–0034 keep their historical vocabulary. [0034](0034-one-vocabulary-from-screen-to-journal.md) and [0037](0037-cycles-and-trials.md) translate it to the terms in `GLOSSARY.md` without changing the tuning decisions.

- [0001: Go as the implementation language](0001-go.md) — **in force** for the language; its Bubble Tea consequence no longer holds: `togi watch` uses a custom redraw loop with Lip Gloss rendering.
- [0002: Clean-room rewrite, not a port of linux-corecycler](0002-clean-room-rewrite.md) — **in force**.
- [0003: The journal is the source of truth; no database](0003-journal-is-source-of-truth.md) — **in force**.
- [0004: togi finds offsets; BIOS applies them](0004-find-only.md) — **in force**.
- [0005: Unattended tuning is a GRUB boot entry](0005-grub-tuning-boot.md) — **in force**.
- [0006: Search starts from the BIOS values](0006-start-from-bios-values.md) — **in force**.
- [0007: Unattributed failures back off as suspects; depth is regained only on request](0007-suspect-backoffs.md) — **superseded** by [0011](0011-blame-by-load-and-automatic-regain.md).
- [0008: Every workload checks its own results](0008-self-checking-workloads.md) — **in force**.
- [0009: Compatibility across updates](0009-compatibility-across-updates.md) — **partly superseded** by [0019](0019-a-ruleset-change-starts-a-seeded-session.md): refusal of older journals.
- [0010: Development programs live in tools/](0010-development-programs-in-tools.md) — **in force**.
- [0011: Blame unattributed failures by load and regain depth automatically](0011-blame-by-load-and-automatic-regain.md) — **superseded** by [0015](0015-avx-512-first-in-confirmation.md): confirmation order; by [0020](0020-hunt-and-refine.md): blame, depth retry and confirmation; by [0038](0038-self-sufficient-cores.md): the R7 CCD and all-core checking step.
- [0012: Releases open with a push and publish from CI](0012-release-by-push.md) — **superseded** by [0014](0014-release-from-a-workflow.md).
- [0013: Candidate edges start a new session in confirmation](0013-candidate-edges-for-a-new-session.md) — **partly superseded** by [0019](0019-a-ruleset-change-starts-a-seeded-session.md): carrying nothing from archives; by [0020](0020-hunt-and-refine.md): candidate-solo-limit confirmation; by [0027](0027-carry-trial-facts.md): proving candidate solo limits only with new-session trials and rejecting carried passes; [0018](0018-crashes-are-not-a-cost.md) counts crash cost as reboot time; configured candidate solo limits remain.
- [0014: Releases are made by a workflow started by hand](0014-release-from-a-workflow.md) — **partly superseded** by [0016](0016-every-pull-request-runs-every-check.md): release checks; by [0033](0033-changelog-fragments.md): reading entries from `[Unreleased]`.
- [0015: Run mprime AVX-512 first in confirmation](0015-avx-512-first-in-confirmation.md) — **superseded** by [0020](0020-hunt-and-refine.md).
- [0016: Every pull request runs every flake check; the release trusts `main`](0016-every-pull-request-runs-every-check.md) — **partly superseded** by [0022](0022-run-the-race-detector-in-ci.md): race-detector decision and consequences; by [0046](0046-macos-checks-the-package-on-main.md): the macOS job runs only on push to `main`.
- [0017: Publish as togi from a fresh repository](0017-publish-as-togi-from-a-fresh-repository.md) — **in force**.
- [0018: Crashes are not a cost](0018-crashes-are-not-a-cost.md) — **in force**.
- [0019: A ruleset change starts a seeded session](0019-a-ruleset-change-starts-a-seeded-session.md) — **partly superseded** by [0020](0020-hunt-and-refine.md): candidate-solo-limit checking and same-ruleset BIOS dead end; by [0027](0027-carry-trial-facts.md): excluding carried passes and unattributed failure facts, the ruleset-based archive walk for them and live rechecking of carried candidate solo limits.
- [0020: Hunt the core behind every failure and refine every core to its mark](0020-hunt-and-refine.md) — **partly superseded** by [0023](0023-hunts-that-converge-on-shared-voltage.md): hunt parked offsets and combination backoff; by [0024](0024-schedule-from-uncontradicted-evidence.md): initial hunt duration, partition order and full cycle after every deepening; by [0028](0028-remove-tiers.md): Bronze, Silver and Gold, the tier clock and failure-rate bounds; by [0038](0038-self-sufficient-cores.md): multi-core R7 hunts, joint/combination backoff, noisy group testing and affected R7 checking/deepening; other regimes' hunt mechanics, full-cycle coverage and deepening remain.
- [0021: Cache check outputs on Cachix](0021-cache-check-outputs-on-cachix.md) — **amended** by [0042](0042-opt-in-to-the-check-cache-locally.md): contributors may trust the cache so that local `just check` downloads unchanged checks; CI's substitution and pushes remain; and by [0045](0045-the-tight-loop-reuses-cached-test-results.md): fresh shuffle seeds come locally from `just gate`, not `just test`.
- [0022: Run the race detector in CI](0022-run-the-race-detector-in-ci.md) — **in force**.
- [0023: Hunts that converge on shared voltage](0023-hunts-that-converge-on-shared-voltage.md) — **superseded** by [0038](0038-self-sufficient-cores.md): measured request attribution, partial chains and voltage-targeted backoff replace multi-core R7 hunts; its hunt mechanics remain outside multi-core R7.
- [0024: Schedule from uncontradicted evidence](0024-schedule-from-uncontradicted-evidence.md) — **partly superseded** by [0027](0027-carry-trial-facts.md): after-reset hunt-planning windows admit eligible carried evidence across their local sequence boundary; by [0028](0028-remove-tiers.md): Bronze credit, tier-change citation and the tier clock; earlier clean-cycle credit for `run --cycles` remains.
- [0025: Recorded agent review](0025-recorded-agent-review.md) — **partly superseded** by [0026](0026-review-only-what-changed.md): a whole record for every reviewed head, version-1 JSON and fresh reviewers for fix diffs; by [0031](0031-pull-requests-under-the-owners-account.md): the App as the agents' identity for every GitHub action; by [0039](0039-remind-only-the-main-session.md): the reminder for every agent except `reviewer`; by [0041](0041-coordinator-reviews-small-pull-requests.md): `reviewer` agents for every pull request; by [0047](0047-agents-merge-green-pull-requests.md): who merges; the App still posts review records and `review` checks.
- [0026: Review only what changed](0026-review-only-what-changed.md) — **partly superseded** by [0030](0030-coverage-is-a-review-signal.md): the record's version-2 JSON, now version 3 with the coverage judgment; reviewing only what changed remains.
- [0027: Carry trial facts across ruleset changes](0027-carry-trial-facts.md) — **partly superseded** by [0038](0038-self-sufficient-cores.md): multi-core R7 failures are processed once for voltage-targeted backoff instead of known-failure skips; carry provenance, exclusions and live-only full-cycle coverage remain.
- [0028: Report qualified rotations instead of durability tiers](0028-remove-tiers.md) — **in force**.
- [0029: Bench verdicts rest on fitted machines](0029-bench-verdicts-rest-on-fitted-machines.md) — **amended** by [0044](0044-write-flagged-target-fits.md): when the all-facts fit fails the model check, flagged refits are written unconstrained with their flags named, and a kept tuner experiment needs target model checks unchanged from setup instead of passing; the check and the rule that a flagged member cannot support a target-machine claim remain.
- [0030: Coverage is a review signal, not a target](0030-coverage-is-a-review-signal.md) — **partly superseded** by [0032](0032-coverage-lists-uncovered-changed-lines.md): whole-file blocks and reviewers matching them to hunks; coverage as a review signal, never a target, remains.
- [0031: Pull requests under the owner's account](0031-pull-requests-under-the-owners-account.md) — **in force**.
- [0032: Coverage lists uncovered changed lines](0032-coverage-lists-uncovered-changed-lines.md) — **in force**.
- [0033: Changelog entries are per-pull-request fragments](0033-changelog-fragments.md) — **in force**.
- [0034: One vocabulary from screen to journal](0034-one-vocabulary-from-screen-to-journal.md) — **in force** for one vocabulary and legacy journal translation; [0037](0037-cycles-and-trials.md) renames the checking schedule and pass-evidence unit and advances the journal to schema 4.
- [0035: License togi under GPL-3.0-or-later](0035-gpl-3.0-or-later.md) — **in force**.
- [0036: Flake checks stay hermetic](0036-flake-checks-stay-hermetic.md) — **in force**.
- [0037: Cycles and trials](0037-cycles-and-trials.md) — **in force**.
- [0038: Self-sufficient cores under shared voltage](0038-self-sufficient-cores.md) — **accepted, in force** for ruleset-9 multi-core R7 checking, attribution, voltage-targeted backoff and deepening scope; **amended** by [0040](0040-located-hunts.md): an unattributed failure is located on the unloaded cores before it is charged, the zero-offset dead end holds irrespective of idle cores only after that and after an all-zero rerun failed too, and the Ruleset 10 gate scores pooled seeds on the median and 90th-percentile worst R7 hazard instead of the maximum and also covers the hand-set scenarios `default`, `idle-limit` and `late-onset`.
- [0039: Remind only the main session](0039-remind-only-the-main-session.md) — **in force**.
- [0040: Locate multi-core R7 failures before charging the loaded cores](0040-located-hunts.md) — **in force**, including the all-zero rerun before every `failure_at_zero` dead end.
- [0041: The coordinator reviews small pull requests](0041-coordinator-reviews-small-pull-requests.md) — **in force**.
- [0042: Opt in to the check cache locally](0042-opt-in-to-the-check-cache-locally.md) — **in force**.
- [0043: Record work that waits on the target machine](0043-record-work-that-waits-on-the-target-machine.md) — **in force**.
- [0044: Write flagged target fits](0044-write-flagged-target-fits.md) — **in force**.
- [0045: The tight loop reuses cached test results](0045-the-tight-loop-reuses-cached-test-results.md) — **in force**.
- [0046: macOS checks only the package, on `main`](0046-macos-checks-the-package-on-main.md) — **in force**.
- [0047: Agents merge green pull requests](0047-agents-merge-green-pull-requests.md) — **in force**.
