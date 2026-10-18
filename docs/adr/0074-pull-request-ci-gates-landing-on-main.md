# Pull request CI gates landing on main

Status: see [the index](README.md).

Supersedes [ADR 0016](0016-every-pull-request-runs-every-check.md)'s check and release gates; amends [ADR 0047](0047-gate-before-every-push.md)'s gate contents and [ADR 0051](0051-light-checks-for-review-fixes.md)'s fix-push checks ([#1141](https://github.com/shgew/togi/issues/1141)).

## Context

Pull requests wait much longer for CI than their checks take. Measurements in #1141 covered 150 `check` runs over roughly fifteen hours:

- Each pull request run started 26 jobs: evaluation, 24 flake checks and the aggregate. The free plan permits 20 concurrent jobs across the account; while busy, CI occupied 19–20 slots for about 160 minutes.
- Median push-to-green time was 27.5 minutes, with a p90 of 43 minutes, against an eight-minute critical path through evaluation and nextest. Queueing accounted for the difference.
- A successful run used about 50 runner-minutes. Five VM tests, their two test executables and four simulator shards used about 34 of them; separate VM runners rebuilt the same package.
- 47% of matrix jobs were Cachix hits that still occupied a runner for setup.
- Of 51 push runs on `main`, 50 were cancelled by the next merge.

Local checks already covered clippy, nextest and the cheap checks: 466 pushes used `just ship` and 476 used `just push-fix` in ten days. Only about 5% of pushes had run the VM tests and simulator suite locally. Of eight pull request failures in five days, two VM tests caught real defects no local check ran; none came from simulator shards. Moving those checks out of pull request CI therefore needs a local gate, not just fewer CI jobs.

## Decision

**Pull request CI is the gate for landing on `main`; the author runs the heavy checks locally and the release runs every check.**

- `check.yml` runs on `pull_request` and `workflow_dispatch`, not on pushes. Pull request mode selects `clippy`, `nextest` and `doc` as separate matrix jobs, plus one `light` job building `fmt`, `changes`, `deny`, `shear`, `module`, `board-test`, `sql-test` and `package` in one `nix build --keep-going`. VM tests, their test executables and simulator shards are not selected.
- Full mode, started by `workflow_dispatch`, selects every `checks.x86_64-linux` attribute, one matrix job per check. Its aggregate verifies that every `sim-suite-<i>` shard declared by `nix/sim-suite.json` was selected, so a missing shard cannot silently pass.
- Evaluation starts build jobs only for outputs absent from the public `togi` Cachix cache: an HTTP 200 from `https://togi.cachix.org/<store-hash>.narinfo` establishes a hit. The aggregate job, named `check`, passes only when every selected check is cached or built successfully, including when no build was needed. A cache hit counts toward full-mode shard coverage.
- The separate `cargo-fmt` flake check is removed. `fmt` runs treefmt, with rustfmt's `--edition 2024`, over every Rust file.
- The release gate dispatches `check.yml` with `{"ref":"main"}` through `POST /repos/{owner}/{repo}/actions/workflows/check.yml/dispatches` and waits for the `workflow_dispatch` run whose `head_sha` is the commit being released. It reuses an existing dispatch run for that SHA rather than dispatching again. `release.yml` grants `actions: write` for this. `just check-main` dispatches the same full run on `main`, follows it and exits non-zero unless it passes.
- `just gate`, and `just push-fix` after its existing steps, build all VM tests (checks with a `driver` attribute) and every `sim-suite-*` shard through `just check-one`. A private `_heavy-checks` recipe prints their names from the flake; Nix rebuilds only changed inputs. The simulator-suite exception to the local gate is gone. Off Linux, the recipes skip both groups and explain that the release's full check runs them. On Linux without a writable `/dev/kvm`, they run the simulator shards, skip the VMs and explain why.
- The target machine's togi input is a release tag, never a bare `main` commit. The `needs-hardware` queue and release refusal remain as [ADR 0043](0043-record-work-that-waits-on-the-target-machine.md) decides.
- Dependabot pull requests get their VM and simulator results only at release. Paid and self-hosted runners stay out.

## Considered Options

- **Keep ADR 0016, adding only the Cachix skip and job packing:** rejected. Those remove runner setup for cached outputs and reduce cheap-job overhead, but changed VM and simulator inputs would still occupy most runner time on each pull request.
- **Run the slow checks nightly on `main`:** rejected. A nightly failure is detached from the change that introduced it, and does not establish that the exact commit being released passed. Local heavy checks and an exact-head release gate provide those two boundaries.
- **Pay for larger runners or use self-hosted runners:** rejected. Larger runners add cost rather than remove duplicate work; self-hosted runners would run public pull request code on private hardware.
- **Use changed paths to decide whether the gate runs VM tests:** rejected. Nix already tracks their inputs exactly and reuses unchanged derivations. A second input map would drift.

## Consequences

- Pull request CI has at most six jobs, including evaluation and aggregation, and fewer on cache hits. It stops competing for slots with VM and simulator work on every push.
- A VM or simulator regression from a Dependabot pull request or an unchecked push can first surface at release. The release refuses rather than shipping that commit.
- `just gate` and `just push-fix` take longer when package or simulator inputs change. Without Linux or usable KVM, their printed skips disclose the checks deferred to release.
- Agents gating at once may build VMs in parallel on one host. [ADR 0072](0072-agents-share-a-host-without-locks.md) still forbids recipe locks; Nix builds each derivation once.
- The target machine runs only release tags, after the full check, rather than following unchecked `main` commits.
