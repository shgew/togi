# macOS checks only the package, on `main`

[ADR 0016](0016-every-pull-request-runs-every-check.md) has every pull request run every flake check and rejected fast checks on pull requests with slow ones on push to `main`, because breakage could still land on `main`. A native `macos-26` arm64 job later joined the `check` workflow, building every `aarch64-darwin` check: `package`, `race`, `lint`, `fmt`, `changes` and `module`. That job became the slowest part of every run.

In [check run 37970253274](https://github.com/shgew/togi/actions/runs/37970253274), on a pull request that changed Go files, the macOS job took 418 seconds, 370 of them in one `nix build` sharing the runner's 3 M1 cores: `race` took 5m00s, `lint` 3m46s and the `package` tests 3m22s. The slowest Linux path, `eval` and then `vm`, took 224 seconds. With every check substituted from Cachix, the macOS job still took 70–116 seconds against about 65 for the Linux side.

Most of that work repeats Linux. `fmt`, `changes` and `module` give the same result on any host; `module` evaluates the x86_64-linux NixOS configuration. `lint` on Linux already lints `darwin/arm64`. `race` on darwin runs the same four concurrent packages as Linux `race` and compiles less of `trial`. Only `package` runs the tests of the darwin and `!linux` code on the macOS kernel and the Nix darwin stdenv. The target machine is x86_64 Linux; macOS is a development host.

## Decision

- The macOS job builds only `checks.aarch64-darwin.package`. The flake keeps all six darwin checks, so `just check` on a Mac is unchanged.
- The macOS job runs on pushes to `main`, not on pull requests. The aggregating `check` job requires it on `main`, so a darwin breakage fails `check` on `main` and the release waits until it is fixed.
- `race` and `lint` reuse `package`'s vendored Go modules, so a host builds them once instead of three times.

This supersedes ADR 0016's rejection of slow checks on push to `main` for the macOS job only; every Linux check still runs on every pull request.

## Considered Options

- **Keep every darwin check on every pull request:** rejected because it roughly doubles a pull request's wait to repeat Linux results.
- **Only `package` on every pull request:** rejected because, at about three minutes uncached on 3 cores, it would still be the longest job of most runs, to cover a few darwin and `!linux` files that seldom change.
- **Linux arm64 runners instead of macOS:** rejected because they reach none of the darwin code, the macOS kernel or the darwin stdenv, only arm64 code generation, and togi never runs on arm64 Linux.
- **A self-hosted Mac runner:** rejected because it runs collaborators' code on a personal machine and fails whenever that machine is offline.

## Consequences

- A change that breaks the darwin build or its tests can merge. It shows as a failed `check` on `main`, which blocks the release until a fix merges. Authors touching `_darwin.go` or `!linux` files run `just check` on a Mac before review.
- The release's wait on `check` for `main` HEAD includes the macOS job.
- On pull requests, the macOS job shows as skipped.
- CI pushes no darwin output from pull requests, and none of the five other darwin checks at all. Local `just check` on a Mac that trusts the cache ([ADR 0042](0042-opt-in-to-the-check-cache-locally.md)) substitutes only `package`, and only for trees that reached `main`; it builds the rest itself.
