# shycler

Finds the deepest per-core Curve Optimizer offsets a Zen 5 desktop CPU sustains, then keeps testing them so the result earns a durability tier: Bronze, Silver, Gold, Platinum.

- Tunes one core at a time, confirms each across several workload regimes, then guards all offsets together for as long as you let it.
- Tests with self-checking workloads (mprime, y-cruncher) across light, heavy, load-step, medium, SMT, idle and all-core regimes.
- Survives crashes: the next run reads its journal, attributes the crash and continues.
- Records every action in a plain-text journal you can read to see what it did and why.
- Reports the offsets; you enter them in BIOS.

## Status

Works today, on a simulated 16-core machine:
- the full tuning lifecycle: per-core search and confirmation, the endless guard, crash resume, tiers, automatic regain and reset;
- reading a session with `status`, `cert`, `events` and the live `watch` dashboard.

Built for real hardware, a Granite Ridge desktop running NixOS with GRUB, and tested piece by piece on one:
- the `ryzen_smu` driver with its verified core-to-slot mapping;
- mprime and y-cruncher trials, each confined to its cores in a systemd scope;
- machine-check detection from the kernel log;
- the NixOS module and the unattended tuning boot, checked in a NixOS VM.

Not yet:
- a full tuning session on real hardware, start to Bronze.

`just sim` runs a simulated session from a source checkout, for development; see [docs/simulating.md](docs/simulating.md).

## Usage

```sh
shycler --version                         # version and git revision of this build
sudo shycler run                          # tune this machine; Ctrl-C stops, the next run resumes
shycler status                            # per-core offsets, tier and clean hours
shycler watch                             # live dashboard of the session
shycler --state-dir <dir> status          # per-core offsets, tier and clean hours
shycler --state-dir <dir> cert            # the certificate: edges to enter in BIOS
shycler --state-dir <dir> events --core 3 # everything that happened to core 3
```

[docs/howto.md](docs/howto.md) walks through installing the NixOS module, a first in-session run and an overnight tuning boot. `shycler --help` lists every command, and `shycler <command> --help` gives its description, examples and flags. [Commands](docs/spec/runtime.md#commands) describes each one in full.

## Documentation

|Document|Read it for|
|---|---|
|[docs/howto.md](docs/howto.md)|Installing shycler and running a tuning session|
|[docs/releasing.md](docs/releasing.md)|Versioning, the release workflow and tags|
|[docs/simulating.md](docs/simulating.md)|Running a simulated session for development|
|[CHANGELOG.md](CHANGELOG.md)|What changed, newest first|
|[CONTEXT.md](CONTEXT.md)|The vocabulary: offsets, phases, regimes, tiers|
|[docs/spec/tuner.md](docs/spec/tuner.md)|How offsets are searched, confirmed, guarded and certified|
|[docs/spec/workloads.md](docs/spec/workloads.md)|The workload regimes and how failures are detected|
|[docs/spec/journal.md](docs/spec/journal.md)|The journal, its events and the state file|
|[docs/spec/runtime.md](docs/spec/runtime.md)|Commands, configuration, the tuning boot and the NixOS module|
|[docs/adr/](docs/adr/)|Why each major decision was made|
|[docs/prior-art.md](docs/prior-art.md)|What was taken from, and left out of, earlier tools|
|[Issues](https://github.com/shgew/shycler/issues)|The plan, ideas and bugs; the `1.0` milestone holds what ships in 1.0|
|[AGENTS.md](AGENTS.md)|Contributing: workflow, commands and conventions|

## Development

Enter the dev shell with `nix develop`, or run `direnv allow` once if you use direnv. `just test` is the tight loop, `just gate` runs lint, formatting checks and tests, and `just ci` adds the race detector and integration tests; it runs on every pull request. `just check` runs every flake check, including the NixOS VM test; the release workflow runs it with `--race`, and `just vuln`, before it publishes. Recipes also work outside the dev shell; run `just` to list them. [AGENTS.md](AGENTS.md) has the rest.

shycler runs on NixOS. Development also works on macOS (aarch64-darwin): the dev shell, the tests, `just sim`, and `status`, `cert`, `events` and `reset` against a copied state directory. `just check` there skips the VM test, and `shycler run` exits with an error.
