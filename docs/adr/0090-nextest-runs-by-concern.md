# Nextest runs by concern

Status: see [the index](README.md).

Supersedes [ADR 0075](0075-nextest-runs-in-dependency-layer-groups.md). Amends [ADR 0047](0047-gate-before-every-push.md)'s gate contents and [ADR 0051](0051-light-checks-for-review-fixes.md)'s fix-push selection. Uses [ADR 0089](0089-heavy-checks-run-on-ci-runners-not-on-the-contributors-machine.md)'s local and CI tiers. Implements [#1424](https://github.com/shgew/togi/issues/1424).

## Context

The five dependency-layer checks separate source invalidation, not contributor concerns. A product edit can rebuild several overlapping test closures and the unshipped tool suites. A local product loop must remain useful on a 16 GiB laptop without dropping the tool and research tests from PR or release gates.

## Decision

Two checks replace the five groups:

- `nextest` is local tier. `productCrates` derives the normal, build and test dependency closure of `togi`, `togi-simrun` and `togi-watchtest`, retaining the shipped product and its simulator/dashboard test support.
- `nextest-tools` is CI tier through `passthru.tier = "ci"`. `toolCrates` registers the unshipped tool and research programs and the libraries used only by them. Evaluation rejects an unknown, repeated or missing workspace member across the two concerns.

Each check keeps its tested crates whole, their test dependency closure as code, nextest's configuration and only the external fixtures registered for those tested crates in `crateFixtures`. Narrowed sources retain target stubs so workspace metadata and named-binary filters remain valid. No compatibility check names remain.

`just test` and `just cover <base>` obtain their package arguments from `nextest.crates`; the flake is the single owner of product selection. `just focus <crate>` still runs any named crate, including unshipped tools, locally. `just check-one nextest-tools` runs the complete tool concern explicitly. The separate CI-tier `nextest-recovery` check and its default exclusions are unchanged.

`just gate`, `just push-fix` and `just check` skip CI-tier checks on every host. PR CI and the release's full dispatch still discover every flake check. The release's additional package/test build discovers both `nextest` itself and `nextest-*`, including tools and recovery.

## Safety evidence

No test or assertion is deleted. The product's automatic SMU transaction, receipt, emergency zeroing, teardown, durability and crash-resume proofs stay local. Additional tool-side journal auditing and simulator-process evidence move with their tool crates; [ADR 0089's ledger](0089-heavy-checks-run-on-ci-runners-not-on-the-contributors-machine.md#safety-evidence-ledger) names the moved safety-related families and surviving local mechanism tests. Tool correctness remains an automatic PR and release gate.

## Consequences

- A product edit rebuilds one test closure rather than several dependency layers. A tool edit is checked in CI, with a focused local recipe available while editing.
- Product fixtures cannot accidentally require the complete research corpus; tool fixtures remain inputs of the tool check.
- Product checks no longer provide local evidence for unshipped tool behavior. Local Clippy and doctests retain ADR 0085's incremental Cargo arrangement.
- These selections reduce duplicate work; they do not bound a test's allocation or promise a peak-memory limit.

## Rejected

- Preserve dependency-layer checks as aliases: that retains duplicate scheduling and two descriptions of the supported interface.
- Delete tool tests or run them only before release: every PR must still prove unshipped tooling that CI and development depend on.
- Select crates from a Git diff: check outputs remain hermetic and represent the complete named concern.
