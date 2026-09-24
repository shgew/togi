# shycler

Finds the deepest per-core Curve Optimizer offsets a Zen 5 desktop CPU sustains, then keeps testing them so the result earns a durability tier: Bronze, Silver, Gold, Platinum.

- Tunes one core at a time, confirms each across several workload regimes, then guards all offsets together for as long as you let it.
- Tests with self-checking workloads (mprime, y-cruncher) across light, heavy, load-step, medium, SMT, idle and all-core regimes.
- Survives crashes: the next run reads its journal, attributes the crash and continues.
- Records every action in a plain-text journal you can read to see what it did and why.
- Reports the offsets; you enter them in BIOS.

## Status

Works today, on a simulated 16-core machine:
- the full tuning lifecycle: per-core search and confirmation, the endless guard, crash resume, tiers, regain and reset;
- reading a session with `status`, `cert` and `events`.

Not yet:
- running on real hardware: the SMU driver, the workload backends, and crash and machine-check detection;
- the NixOS module and the unattended tuning boot;
- CI.

## Usage

```sh
shycler run --sim 1                       # simulate a session; prints its state directory
shycler --state-dir <dir> status          # per-core offsets, tier and clean hours
shycler --state-dir <dir> cert            # the certificate: edges to enter in BIOS
shycler --state-dir <dir> events --core 3 # everything that happened to core 3
```

`shycler --help` lists every command, and `shycler <command> --help` gives its description, examples and flags. [Commands](docs/spec/runtime.md#commands) describes each one in full.

## Documentation

|Document|Read it for|
|---|---|
|[CHANGELOG.md](CHANGELOG.md)|What changed, newest first|
|[CONTEXT.md](CONTEXT.md)|The vocabulary: offsets, phases, regimes, tiers|
|[docs/spec/tuner.md](docs/spec/tuner.md)|How offsets are searched, confirmed, guarded and certified|
|[docs/spec/workloads.md](docs/spec/workloads.md)|The workload regimes and how failures are detected|
|[docs/spec/journal.md](docs/spec/journal.md)|The journal, its events and the state file|
|[docs/spec/runtime.md](docs/spec/runtime.md)|Commands, configuration, the tuning boot and the NixOS module|
|[docs/adr/](docs/adr/)|Why each major decision was made|
|[docs/prior-art.md](docs/prior-art.md)|What was taken from, and left out of, earlier tools|
|[docs/ROADMAP.md](docs/ROADMAP.md)|Plan to 1.0|
|[AGENTS.md](AGENTS.md)|Contributing: workflow, commands and conventions|

## Development

Enter the dev shell with `nix develop`, or run `direnv allow` once if you use direnv. `go test ./...` is the tight loop, and `nix flake check` runs tests and lint. [AGENTS.md](AGENTS.md) has the rest.
