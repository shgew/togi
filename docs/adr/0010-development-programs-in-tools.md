# Development programs live in tools/

A simulated session answers nothing about the operator's chip: its edges are drawn from a seed. It exists to exercise the run loop, reproduce bugs and produce journals for tests. Kept as `shycler run --sim`, it put a developer harness in the operator CLI and shaped `run` for everyone, with a second `--rotations` default and a `--sim`/`--tuning-boot` exclusion.

## Decision

Development and debugging behavior lives in programs under `tools/`, never in `cmd/shycler`. The package builds only `cmd/shycler`, so no development program ships. `tools/release` already worked this way; the simulator moves to `tools/sim` (`go run ./tools/sim`, wrapped by `just sim`), and the crash-reboot loop it drives moves from `internal/session` to `internal/simrun`, so `session` no longer imports `sim`.

Comparable projects keep such harnesses out of the binary operators install: TigerBeetle's [VOPR](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/internals/vopr.md) is its own build target, Kubernetes' [Kubemark](https://github.com/kubernetes/kubernetes/blob/master/cmd/kubemark/hollow-node.go) is a separate binary, and OpenZFS ships [ztest](https://openzfs.github.io/openzfs-docs/man/master/1/ztest.1.html) apart from its tools.

## Considered Options

- **A build-tagged `shycler debug` command,** as etcd builds failpoints in with [gofail](https://github.com/etcd-io/gofail): rejected because it keeps development code in the operator command's package and adds a second build of the same binary to keep working.
- **Simulation inside the release binary,** as [FoundationDB](https://apple.github.io/foundationdb/testing.html) and `vault server -dev` do: rejected because a simulated session tells the operator nothing about their machine, and its flags and exclusions complicate the commands they do use.

## Consequences

Simulating needs a source checkout. The operator-facing `shycler` has no simulation flags, and a future operator diagnostic such as `shycler doctor` stays free of simulation. Tests that need a simulated journal build it in process through `internal/simrun`.
