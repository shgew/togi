# Development programs live in tools/

A simulated session answers nothing about the operator's chip: its edges are drawn from a seed. It exists to exercise the run loop, reproduce bugs and produce journals for tests. Kept as `togi run --sim`, it put a developer harness in the operator CLI and shaped `run` for everyone, with a second `--rotations` default and a `--sim`/`--tuning-boot` exclusion.

## Decision

Development and debugging behavior lives in programs under `tools/`, never in `cmd/togi`. The package builds only `cmd/togi`, so no development program ships. `tools/release` already worked this way; the simulator moves to `tools/sim` (`go run ./tools/sim`, wrapped by `just sim`), and the crash-reboot loop it drives moves from `internal/session` to `internal/simrun`, so `session` no longer imports `sim`.

Comparable projects keep such harnesses out of the binary operators install: TigerBeetle's [VOPR](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/internals/vopr.md) is its own build target, Kubernetes' [Kubemark](https://github.com/kubernetes/kubernetes/blob/master/cmd/kubemark/hollow-node.go) is a separate binary, and OpenZFS ships [ztest](https://openzfs.github.io/openzfs-docs/man/master/1/ztest.1.html) apart from its tools.

## Considered Options

- **A build-tagged `togi debug` command,** as etcd builds failpoints in with [gofail](https://github.com/etcd-io/gofail): rejected because it keeps development code in the operator command's package and adds a second build of the same binary to keep working.
- **Simulation inside the release binary,** as [FoundationDB](https://apple.github.io/foundationdb/testing.html) and `vault server -dev` do: rejected because a simulated session tells the operator nothing about their machine, and its flags and exclusions complicate the commands they do use.

## Consequences

Simulating needs a source checkout. The operator-facing `togi` has no simulation flags, and a future operator diagnostic such as `togi doctor` stays free of simulation. Tests that need a simulated journal build it in process through `internal/simrun`.

## Amendment (#918)

The mechanism after the Rust cutover; the decision and its reasons are unchanged. Development programs are the `togi-tool-*` crates (`publish = false`; `just sim` runs `cargo run --release --locked -p togi-tool-sim`), never part of the `togi` crate, and `togi` depends on none of them. The package builds `-p togi` only. The crash-reboot loop is the `togi-simrun` crate, which `togi-session` does not use; the `togi` crate and `togi-session` take `togi-sim` as a dev-dependency only, and tests that need a simulated journal build it in process through `togi-simrun` (via the `togi-watchtest` dev-dependency). `tools/` keeps the benchmark data (`tools/bench`), the Nushell `board` and the three retained Go programs, `release`, `reviews` and `cover`, which also never ship.

## Amendment (#630)

[ADR 0071](0071-no-go-in-the-repository.md) rewrites the Go programs; the decision and its reasons are unchanged. The release tool is the `togi-tool-release` crate (`just release-preview` runs `cargo run --quiet --locked -p togi-tool-release`) and the review tool the `togi-tool-reviews` crate (`just reviews`). `tools/` keeps the benchmark data, the Nushell `board`, the `just sql` views and the retained Go program, `cover`, until its Rust rewrite lands.

## Amendment (#633, #1038)

The coverage tool is the `togi-tool-cover` crate (`just cover`), the last Go program; the decision and its reasons are unchanged. `tools/` keeps the benchmark data, the Nushell `board` and the `just sql` views, and no Go remains in the repository.
