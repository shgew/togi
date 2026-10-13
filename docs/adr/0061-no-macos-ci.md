# No CI job runs on macOS

Status: see [the index](README.md).

Supersedes [ADR 0046](0046-macos-checks-the-package-on-main.md) and the macOS decision of [ADR 0057](0057-running-the-rust-migration-unattended.md), which [ADR 0058](0058-native-rust-rewrite.md) kept. The owner decided it on 2026-10-13 ([#930](https://github.com/shgew/togi/issues/930), under [#902](https://github.com/shgew/togi/issues/902)).

## Context

ADR 0046 added a native `macos-26` arm64 job to the `check` workflow, on pushes to `main` only, that built `checks.aarch64-darwin.package`. ADR 0057 added `checks.aarch64-darwin.rust-nextest` to it, to build the Rust workspace and run its tests on the Mac. The aggregating `check` job required the macOS job on `main`, so a darwin breakage failed `check` there and the release waited.

The job became the slowest part of every push to `main`. In [run 38405568483](https://github.com/shgew/togi/actions/runs/38405568483) the `rust-nextest` step alone took 14 min 28 s, nearly all of it compiling the Rust tests for macOS from scratch. Pull requests never ran the job, so nothing waited on it but releases. The target machine is x86_64 Linux; macOS is a development host, and breakages found there remain bugs to fix.

## Decision

- **No CI job runs on macOS.** The `macos` job leaves `.github/workflows/check.yml`, and the `check` job requires only the Linux checks and the simulator-shard guard.
- **The cross-platform check stays: `cargo clippy --target aarch64-apple-darwin`**, run from Linux by `just lint`, `just gate` and the `clippy` flake check. It type-checks the non-Linux code paths but does not run them.
- **Code behind `cfg(target_os = "linux")` keeps a working non-Linux path.**
- **A pull request that changes platform-gated code says that it was not built on macOS**, unless its author built it there.
- **A breakage found on macOS is filed and fixed like any other bug.**
- **Only the newest `main` check run needs to finish** ([#941](https://github.com/shgew/togi/issues/941)). A newer push cancels its predecessor, as pull request pushes already do; ADR 0016's rule that every push to `main` runs to the end is superseded. A release still requires a successful check on its exact head; if a merge cancels the run it is waiting for, rerun the release on the new head ([Releasing](../releasing.md)).
- The flake keeps its `aarch64-darwin` outputs, so `just check` on a Mac still works as a development tool; nothing in CI builds or caches them.

## Considered Options

- **Keep the job on `main`:** rejected by the owner: about 14 minutes per push, for a breakage the owner would notice anyway.
- **Linux arm64 runners instead of macOS:** rejected as in ADR 0046, because they reach none of the darwin code and togi never runs on arm64 Linux.
- **A self-hosted Mac runner:** rejected as in ADR 0046, because it runs collaborators' code on a personal machine and fails whenever that machine is offline.

## Consequences

- A change that breaks the macOS build or its tests can merge and release. Clippy for `aarch64-apple-darwin` catches type errors in the darwin paths before then, but not a failing test, a link error or a runtime difference of the macOS kernel.
- Pushes to `main` and releases no longer wait on a macOS job.
- CI pushes no darwin output to Cachix. A local `just check` on a Mac that trusts the cache ([ADR 0042](0042-opt-in-to-the-check-cache-locally.md)) substitutes a darwin check only when an output for its inputs was pushed earlier, and builds the rest itself.
- Every Linux check still runs on every pull request ([ADR 0016](0016-every-pull-request-runs-every-check.md)). ADR 0048's decisions and the rest of ADRs 0057 and 0058 stand.
