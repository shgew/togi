# Architecture decisions

The specs describe current behavior; ADRs record why and the alternatives rejected; where they disagree, the spec wins and this index is out of date.

A new ADR adds its line here and updates the status of any ADR it supersedes.

ADRs 0001–0033 keep their historical vocabulary. [0034](0034-one-vocabulary-from-screen-to-journal.md) translates it to the terms in `GLOSSARY.md` without changing the tuning decisions.

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
- [0011: Blame unattributed failures by load and regain depth automatically](0011-blame-by-load-and-automatic-regain.md) — **partly superseded** by [0015](0015-avx-512-first-in-confirmation.md): confirmation order; by [0020](0020-hunt-and-refine.md): blame, depth retry and confirmation; the R7 CCD and all-core checking step remains.
- [0012: Releases open with a push and publish from CI](0012-release-by-push.md) — **superseded** by [0014](0014-release-from-a-workflow.md).
- [0013: Candidate edges start a new session in confirmation](0013-candidate-edges-for-a-new-session.md) — **partly superseded** by [0019](0019-a-ruleset-change-starts-a-seeded-session.md): carrying nothing from archives; by [0020](0020-hunt-and-refine.md): candidate-solo-limit confirmation; by [0027](0027-carry-trial-facts.md): proving candidate solo limits only with new-session starts and rejecting carried passes; [0018](0018-crashes-are-not-a-cost.md) counts crash cost as reboot time; configured candidate solo limits remain.
- [0014: Releases are made by a workflow started by hand](0014-release-from-a-workflow.md) — **partly superseded** by [0016](0016-every-pull-request-runs-every-check.md): release checks; by [0033](0033-changelog-fragments.md): reading entries from `[Unreleased]`.
- [0015: Run mprime AVX-512 first in confirmation](0015-avx-512-first-in-confirmation.md) — **superseded** by [0020](0020-hunt-and-refine.md).
- [0016: Every pull request runs every flake check; the release trusts `main`](0016-every-pull-request-runs-every-check.md) — **partly superseded** by [0022](0022-run-the-race-detector-in-ci.md): race-detector decision and consequences.
- [0017: Publish as togi from a fresh repository](0017-publish-as-togi-from-a-fresh-repository.md) — **in force**.
- [0018: Crashes are not a cost](0018-crashes-are-not-a-cost.md) — **in force**.
- [0019: A ruleset change starts a seeded session](0019-a-ruleset-change-starts-a-seeded-session.md) — **partly superseded** by [0020](0020-hunt-and-refine.md): candidate-solo-limit checking and same-ruleset BIOS dead end; by [0027](0027-carry-trial-facts.md): excluding carried passes and unattributed failure facts, the ruleset-based archive walk for them and live rechecking of carried solo limits.
- [0020: Hunt the core behind every failure and refine every core to its mark](0020-hunt-and-refine.md) — **partly superseded** by [0023](0023-hunts-that-converge-on-shared-voltage.md): hunt parked offsets and combination backoff; by [0024](0024-schedule-from-uncontradicted-evidence.md): initial hunt duration, partition order and full lap after every deepening; by [0028](0028-remove-tiers.md): Bronze, Silver and Gold, the tier clock and failure-rate bounds; full-lap coverage and deepening remain.
- [0021: Cache check outputs on Cachix](0021-cache-check-outputs-on-cachix.md) — **in force**.
- [0022: Run the race detector in CI](0022-run-the-race-detector-in-ci.md) — **in force**.
- [0023: Hunts that converge on shared voltage](0023-hunts-that-converge-on-shared-voltage.md) — **in force**.
- [0024: Schedule from uncontradicted evidence](0024-schedule-from-uncontradicted-evidence.md) — **partly superseded** by [0027](0027-carry-trial-facts.md): after-reset hunt-planning windows admit eligible carried evidence across their local sequence boundary; by [0028](0028-remove-tiers.md): Bronze credit, tier-change citation and the tier clock; earlier clean-lap credit for `run --laps` remains.
- [0025: Recorded agent review](0025-recorded-agent-review.md) — **partly superseded** by [0026](0026-review-only-what-changed.md): a whole record for every reviewed head, version-1 JSON and fresh reviewers for fix diffs; by [0031](0031-pull-requests-under-the-owners-account.md): the App as the agents' identity for every GitHub action; the App still posts review records and `review` checks.
- [0026: Review only what changed](0026-review-only-what-changed.md) — **partly superseded** by [0030](0030-coverage-is-a-review-signal.md): the record's version-2 JSON, now version 3 with the coverage judgment; reviewing only what changed remains.
- [0027: Carry trial facts across ruleset changes](0027-carry-trial-facts.md) — **in force**.
- [0028: Report qualified rotations instead of durability tiers](0028-remove-tiers.md) — **in force**.
- [0029: Bench verdicts rest on fitted machines](0029-bench-verdicts-rest-on-fitted-machines.md) — **in force**.
- [0030: Coverage is a review signal, not a target](0030-coverage-is-a-review-signal.md) — **partly superseded** by [0032](0032-coverage-lists-uncovered-changed-lines.md): whole-file blocks and reviewers matching them to hunks; coverage as a review signal, never a target, remains.
- [0031: Pull requests under the owner's account](0031-pull-requests-under-the-owners-account.md) — **in force**.
- [0032: Coverage lists uncovered changed lines](0032-coverage-lists-uncovered-changed-lines.md) — **in force**.
- [0033: Changelog entries are per-pull-request fragments](0033-changelog-fragments.md) — **in force**.
- [0034: One vocabulary from screen to journal](0034-one-vocabulary-from-screen-to-journal.md) — **in force** for vocabulary, configuration names and schema-3 journal translation.
