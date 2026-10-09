# The tight loop reuses cached test results

`just test` and `just focus` ran `go test -shuffle=on`. Go caches a package's test result only when every flag on the command line is one of its cacheable test flags (`go help test`), and `-shuffle` is not one of them. Every run therefore relinked every test binary into Go's build work directory and reran every package, however little had changed since the last run.

[#151](https://github.com/shgew/togi/issues/151) proposed the same change and was closed because [ADR 0021](0021-cache-check-outputs-on-cachix.md) left local `just test` as the run that draws a fresh shuffle seed when CI substitutes a cached check. Since then `just gate`, required before every commit, runs the integration-tagged tests and the race run with `-shuffle=on` on every invocation, so it draws that seed instead.

Measured on a 32-thread x86_64 Linux machine, rerunning the whole suite with nothing changed and the build cache warm:

| Command | Wall | Written to disk with the temporary directory on btrfs | Packages from cache |
|---|---|---|---|
| `go test -shuffle=on ./...` | 13.5 s | about 800 MB | 0 |
| `go test ./...` | 1.5 s | 18 MB | 38 of 38 |

With the temporary directory on tmpfs, the shuffled rerun took 5.6 s and wrote about 10 MB to disk; nearly all of the disk writes came from the linked test binaries, not from the tests' own temporary files.

## Decision

- `just test` and `just focus` run `go test` without `-shuffle=on`, so a package whose test binary, flags, files and environment are unchanged reuses its cached result.
- `just gate` keeps `-shuffle=on` for the integration-tagged tests and the race run, and the `package` and `race` flake checks keep it in CI. Order-dependent tests are found before every commit and on every pull request.

## Considered Options

- **Keep shuffling in the tight loop:** rejected. It finds order dependence on every edit at the cost of rerunning and relinking the whole suite, and `just gate` already shuffles before every commit.
- **Shuffle with a fixed seed:** rejected. Any `-shuffle` value makes the run uncacheable, so a fixed seed keeps the cost and loses the new orderings.

## Consequences

- A test that passes only in source order is caught by `just gate` or CI, not by `just test`. Both print the `-test.shuffle` seed of a failing run, which `go test -shuffle=SEED` replays.
- `just test` reports `(cached)` for unchanged packages. `just test -count=1` reruns them all.
- `just gate` still relinks and reruns every package on each run.
