# The repository bounds its own build and test parallelism

Status: see [the index](README.md).

Relates to [ADR 0086](0086-heavy-runs-wait-for-memory.md); admission across unrelated commands and the parallelism within one command are separate concerns. Amends the invocation mechanics of [ADR 0085](0085-the-gate-publishes-and-pull-request-ci-runs-every-check.md), not its check inventory or publication policy. Implements [#1324](https://github.com/shgew/togi/issues/1324).

## Context

A contributor's 16 GiB laptop must retain room for the editor and operating system. Cargo, nextest and Nix otherwise choose independent CPU-based parallelism. Nix can run several builds, each using all cores, and coverage runs its own nextest invocation. Limiting only the gate leaves `just check`, direct test runs and coverage unbounded.

## Decision

`tools/check-budget.sh` computes one positive integer: the smaller of the host CPU count and physical memory divided by 2 GiB, rounded down, with a minimum of one. Linux uses `nproc` and `MemTotal` from `/proc/meminfo`; macOS uses `sysctl hw.ncpu` and `hw.memsize`. `TOGI_JOBS` is the single override and must be a positive decimal integer.

The justfile exports that budget as `TOGI_JOBS`, `CARGO_BUILD_JOBS`, `NEXTEST_TEST_THREADS` and `RUST_TEST_THREADS` (libtest/doctest workers). Nested recipes reuse it. This includes `test`, `focus`, `lint`, `cover`, `gate`, `check`, `check-one`, `ship` and `push-fix`; coverage's `cargo llvm-cov nextest` reads the same test-thread environment setting as a direct nextest run. `land` only polls GitHub and builds nothing. `hardware` keeps its explicit single-worker override for shared hardware.

Repository Nix check invocations set `--max-jobs 1 --cores BUDGET`. Only one derivation builds at a time, and its advertised build cores do not exceed the budget. Nix nextest and doctest checks explicitly use `NIX_BUILD_CORES` as their test-thread count; the sandbox does not inherit the justfile's environment. Host Nix configuration is unchanged, and CI retains its runner's Nix core policy.

This is worker admission, not a memory limit. A test may need more than 2 GiB, child processes can allocate independently, and a test's own internal pool is not bounded by nextest's worker count. This decision neither removes a test nor changes assertions or the safety-evidence tier.

## Consequences

Independent local entry points no longer multiply their own worker counts within one invocation. A larger host automatically gets a larger budget, and a contributor can lower it without editing host settings. Serial derivation admission trades some throughput for a predictable product of Nix build slots and per-build cores.

A deliberately supplied Cargo or nextest command-line worker flag still takes precedence over its environment default. The supported repository-wide tuning knob is `TOGI_JOBS`.

## Rejected

- Host-wide Nix defaults: they affect unrelated repositories and do not bound Cargo or coverage.
- Separate Cargo, nextest and Nix tuning knobs: their combined admission is easy to overlook.
- Limiting only `gate`: direct `test`, `cover` and `check` would retain the bypass.
- A memory guarantee based solely on worker count: individual tests and their internal pools have different memory costs.
