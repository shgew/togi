# Run the race detector in CI

Status: supersedes only the race-detector decision and its consequences in [ADR 0016](0016-every-pull-request-runs-every-check.md).

ADR 0016 removed the race detector because its first CI job took 8m00s, against 3m41s for the VM test. Containment and session ownership now add concurrency that should be checked before it merges. [ADR 0021](0021-cache-check-outputs-on-cachix.md) also lets unchanged checks pass by substituting their outputs rather than running again.

## Decision

- Add a `race` flake check on every pull request and push to `main`, running `internal/trial`, `internal/session`, `internal/journal` and `internal/watch` with `-race -shuffle=on`, including their `integration` tests on Linux. It uses the package check's source, pinned Go toolchain and vendored dependencies, without building a separate CLI binary.
- Keep the check's revision `dev` and let the existing eval job discover it. The existing Cachix build jobs substitute and publish its output like any other check; changing docs alone does not rerun it.
- Keep `just test -race` for local runs. `just check-one race` builds the same check CI runs.

## Measurement

The uncached `vm` build in [check run 36645173270](https://github.com/shgew/togi/actions/runs/36645173270) took 167 seconds (23:26:44–23:29:31 UTC on 2026-09-29); the whole job took 180 seconds. This is the CI time budget for selecting the whole suite rather than only the packages with real concurrency.

The whole-suite race build in [check run 36694448962](https://github.com/shgew/togi/actions/runs/36694448962) passed but took 303 seconds (09:11:35–09:16:38 UTC on 2026-09-30); the whole job took 314 seconds. Its check phase took 4m48s. That exceeds both the uncached VM build and job, so the automated check covers the four packages with real concurrency instead. The VM job in the same run took 29 seconds because it substituted an unchanged check; comparing that cache hit to a fresh race build would not measure the cost of running the checks.

## Considered Options

- **The whole suite on every pull request:** rejected after its fresh CI build exceeded the uncached VM job's time. `just test -race` retains whole-suite local coverage.
- **A scheduled run:** still rejected because inputs are pinned and do not change between pushes. The check belongs on the change that could introduce a race.
- **Only local race runs:** rejected because a concurrency regression should prevent a merge, not depend on a developer remembering a separate command.

## Consequences

- A race fails the aggregating `check` job before merging, with no runtime or journal changes.
- The sandbox builds the race-instrumented Go runtime from a cold Go build cache when the check changes. An unchanged check can instead be substituted from Cachix, including after a squash merge.
- The check exercises the four concurrent packages on Linux and is available locally on aarch64-darwin; hardware-tagged tests remain opt-in and are not run by it.
