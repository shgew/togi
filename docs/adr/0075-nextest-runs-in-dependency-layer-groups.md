# Nextest runs in dependency-layer groups

Status: see [the index](README.md).

Superseded by [ADR 0090](0090-nextest-runs-by-concern.md). The groups and measurements below describe the earlier decision.

Amends [ADR 0074](0074-pull-request-ci-gates-landing-on-main.md)'s pull request job set, [ADR 0047](0047-gate-before-every-push.md)'s gate contents and [ADR 0051](0051-light-checks-for-review-fixes.md)'s fix-push tests ([#1146](https://github.com/shgew/togi/issues/1146)).

## Context

`nextest` was the slowest pull request job: 5 min 25 s in run 39116447816 on a 4-vCPU runner, of which about 35 s was setup and substitution, 3 min 02 s compiling the workspace crates and 101 s running 8,201 tests. Both phases kept the four cores busy: every crate had started compiling 57 s in, and the tests summed to 402 CPU seconds. No single test took longer than 7.4 s, so neither a critical path nor one slow test set the time.

The check was one derivation over every crate and test fixture. An edit to any crate, even `togi-tool-reviews`, whose dependents' tests are 1% of the test time, compiled and ran the whole suite. Of the 78 merges before the change that touched Rust, 48 changed crates only. The crate graph is bottom-heavy: a change to `togi-machine` reaches 96% of the test time, `togi-journal` 90%, `togi-tuner` 77%.

## Decision

**`checks.nextest` becomes one `nextest-<group>` check per dependency-layer group, each on a source narrowed to what its tests compile and read.**

- `flake.nix` names the groups in `nextestGroups`: `core` (the journal, tuner, simulator and the libraries under them), `platform` (commands, the host lock, backends, SMU, detection, trials and hardware), `session` (the session, simulation runner, dashboard and `togi`), `tools` (the simulator and analysis tools) and `repo` (`togi-tool-reviews`, `-release`, `-cover`). Evaluation refuses a workspace member in no group or in two, and an empty group.
- Each check runs `cargo nextest run --profile ci -p …` for its crates on `nix/sources.nix`'s source with the group's crates whole, their dev closure as code, and only the fixtures from outside `crates/` that `crateFixtures` lists for them. A change outside a group's inputs leaves its output in Cachix, and evaluation skips it.
- Narrowed sources stub every target path they leave out, discovered ones included, so `cargo metadata`, and with it nextest's filters such as the `binary(hardware)` override, matches the full workspace in every group.
- Pull request mode selects every `nextest-*` check as its own matrix job. The release builds them all on the release commit.
- `just gate` and `just push-fix` build the same `nextest-*` checks through `just check-one` instead of running `cargo nextest` on the workspace or on the changed crates. They prove what CI will build, a missing group entry or fixture included, and Nix reruns only the groups whose inputs changed; unchanged groups come from the local store or Cachix. `push-fix` thereby also reruns the tests of the crates that depend on a changed one, which its changed-crates list left out. `just test` and `just focus` stay `cargo nextest` for the edit loop.

The groups together list the same 8,201 tests and 2 skipped as the single check, by binary, name, filter status and ignored flag.

## Considered Options

- **One check per crate:** rejected. It focuses best, but each derivation compiles its own closure again: 1,322 CPU seconds of local compile against 366 for the workspace, and a modeled mean of about 1,460 runner-seconds and 23 jobs a pull request, above the free plan's 20 concurrent jobs.
- **Build once, partition the run across runners** (crane's `partitions`): rejected. It splits only the 101 s run; the 3-minute compile stays serial.
- **Select tests from the git diff** (nextest's `rdeps()` in one job): rejected. It depends on the base commit, so the check stops being hermetic, and its cached result no longer means the suite passed. Nix already tracks each group's inputs exactly; [ADR 0074](0074-pull-request-ci-gates-landing-on-main.md) rejected a second input map for the same reason.
- **A shared workspace-library artifact for the groups:** rejected for now. It would serialize the groups behind one more derivation and store another target archive, which [ADR 0021](0021-cache-check-outputs-on-cachix.md)'s trim removed.

## Consequences

- A change under every group runs the five on parallel runners; the modeled longest is `session`, about 153 s. A change to a leaf runs only its group: about 63 s for `repo`. Over the 78 merges the modeled mean is 146 s against 325 s. These are local measurements scaled to the runner; the pull request's own runs are the first CI measurements.
- The groups compile shared libraries again: 548 CPU seconds of local compile against 366, and about 28% more runner time a pull request on average.
- Pull request CI has at most ten jobs, up from six.
- In the gate, unchanged groups cost an evaluation: 1.3 s against 13.0 s for a warm `cargo nextest run` of the workspace, measured on a 32-thread development host. A changed group compiles its crates from scratch, with no cargo incremental state: rebuilding all five took 49 s there.
- `Cargo.lock`, the workspace manifests, the toolchain and nextest's profiles are inputs of every group, so a change to them runs all five. When `cargo-deps` is not yet in Cachix, each group builds it on its own runner.
- A new crate names its group, and a test that reads a new file outside `crates/` names it in `crateFixtures`; a fixture left out fails that group's build.
