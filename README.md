# togi

> [!WARNING]
> togi is pre-1.0. It writes Curve Optimizer offsets to your CPU through `ryzen_smu`, and finding each core's solo limit means running it until it fails: expect crashes, reboots and lost work in anything else running. A new version can refuse to continue a session written by an older one. Run it at your own risk.

Finds the deepest per-core Curve Optimizer offsets a Zen 5 desktop CPU sustains, then keeps testing them through full checking cycles. Choose how long to keep testing with `run --cycles N`, or let checking continue indefinitely.

- Searches each core alone, hunts the cores behind unattributed failures, then deepens the profile to the most total depth its failure points and combinations allow before continuing checking.
- Tests with self-checking workloads (mprime, y-cruncher) across light, heavy, load-step, medium, SMT, idle and all-core regimes.
- Survives crashes: the next run reads its journal, attributes the crash and continues.
- Records every action in a plain-text journal you can read to see what it did and why.
- Reports the offsets; you enter them in BIOS.

*togi* (研ぎ) is Japanese for polishing a blade: coarse stones first, then fine ones, until the edge shows.

## Status

Works today, on a simulated 16-core machine:
- the full simulated tuning lifecycle: per-core search, failure hunts, combinations, deepening together, full checking cycles, crash resume and reset;
- a seeded session after a ruleset update or BIOS change: a ruleset change carries eligible same-BIOS trial facts for candidate-solo-limit checks, hunt groups, reruns and deepening, while full-cycle requirements count only live passes; a BIOS change carries solo limits but not failure points or trial facts;
- evidence-based hunt duration and singleton-probe scheduling, and credit for an earlier uncontradicted full cycle at an equal or deeper profile;
- record-only R7 checking parts that leave each CCD's shallowest cores idle, preserving partial-load outcomes as facts without changing offsets, failure points and combinations or full-cycle coverage;
- reading a session's hunt, combinations, clean cycles and valid per-workload trials with `status`, `events` and the live `watch` dashboard; `status` shows the Tctl peak from passes together since the last profile change and its source trial.

Built for real hardware, a Granite Ridge desktop running NixOS with GRUB, and tested piece by piece on one:
- the `ryzen_smu` driver on full 8-core CCDs only; CCDs with fused-off slots are not yet supported;
- mprime and y-cruncher trials, each confined to its cores in a systemd scope;
- machine-check detection from the kernel log;
- the NixOS module and the unattended tuning boot, with two service restart-limit retry boots before returning to the normal system and durable leave reasons; a stubbed NixOS VM check covers the normal-boot fallback.

Not yet:
- a full tuning session on real hardware, from search through a clean cycle.

`just sim` runs a simulated session from a source checkout, for development; see [docs/simulating.md](docs/simulating.md).

## Usage

```sh
togi --version                         # version and git revision of this build
sudo togi run                          # tune this machine; Ctrl-C stops, the next run resumes
sudo togi run --cycles 3            # stop after three clean cycles when deepening is complete
togi status                            # activity, failure points and combinations, per-core offsets and evidence
togi watch                             # full-width dashboard: current trial, tuner forecasts, every core
togi --state-dir <dir> status          # inspect a copied journal
togi --state-dir <dir> events --core 3 # everything that happened to core 3
```

The dashboard adapts to wide and compact terminals. It shows checking's cycle checklist, hunt parts and member probes, or search turns beside recent decisions; `?` explains the gauges and `l` opens the journal. Outcome lines come from the tuner itself. R6 idle trials hold the screen still, without a ticking clock, until the journal records their end.

[docs/howto.md](docs/howto.md) walks through installing the NixOS module, a first in-session run and an overnight tuning boot. `togi --help` lists every command, and `togi <command> --help` gives its description, examples and flags. [Commands](docs/spec/runtime.md#commands) describes each one in full.

## Documentation

|Document|Read it for|
|---|---|
|[docs/howto.md](docs/howto.md)|Installing togi and running a tuning session|
|[docs/releasing.md](docs/releasing.md)|Versioning, the release workflow and tags|
|[docs/simulating.md](docs/simulating.md)|Running a simulated session for development|
|[docs/reviewing.md](docs/reviewing.md)|Reviewing current and archived runs with `just stats`, and writing a run's retro|
|[docs/benchmarking.md](docs/benchmarking.md)|Comparing tuner changes across simulated machines|
|[CHANGELOG.md](CHANGELOG.md)|What changed, newest first|
|[GLOSSARY.md](GLOSSARY.md)|The vocabulary: offsets, phases, regimes, clean cycles|
|[docs/spec/tuner.md](docs/spec/tuner.md)|How offsets are searched, hunted, deepened and checked|
|[docs/spec/workloads.md](docs/spec/workloads.md)|The workload regimes and how failures are detected|
|[docs/spec/journal.md](docs/spec/journal.md)|The journal, its events and the state file|
|[docs/spec/runtime.md](docs/spec/runtime.md)|Commands, configuration, the tuning boot and the NixOS module|
|[docs/adr/](docs/adr/)|Why each major decision was made|
|[docs/prior-art.md](docs/prior-art.md)|What was taken from, and left out of, earlier tools|
|[Issues](https://github.com/shgew/togi/issues)|The plan, ideas and bugs; the `1.0` milestone holds what ships in 1.0|
|[CONTRIBUTING.md](CONTRIBUTING.md)|What contributions are accepted before 1.0|
|[AGENTS.md](AGENTS.md)|Working on togi: workflow, commands and conventions|

## Contributing

Bug reports are welcome as issues. Pull requests and feature requests are not taken before 1.0; [CONTRIBUTING.md](CONTRIBUTING.md) says why.

## License

Copyright (C) 2026 Hleb Shauchenka. togi is free software under the [GNU General Public License, version 3 or any later version](LICENSE) (GPL-3.0-or-later). Releases up to and including 0.9.0 were published under the MIT license and remain available under it ([ADR 0035](docs/adr/0035-gpl-3.0-or-later.md)).

## Development

Enter the dev shell with `nix develop`, or run `direnv allow` once if you use direnv. `just test` is the tight loop, and `just gate` runs every non-VM flake check sequentially, using warm Go caches: formatting, lint, the NixOS module's evaluation, changelog fragments, the vendored Go modules, shuffled integration-tagged tests, the race detector and, on Linux, the hardware-tagged trial test compile. `just check` adds the Linux VM tests and stays CI's definition of green; CI runs every check on every pull request. Recipes also work outside the dev shell; run `just` to list them. [AGENTS.md](AGENTS.md) has the agent check ladder and the rest.

Hardware tests share a private host lock with `run` and `reset`. Delegated users need explicit lock access as well as SMU and cpuset-controller permissions; see [host-lock provisioning](docs/howto.md#host-lock-and-delegated-hardware-tests). After upgrading from a public-readable lock, quiesce old lock openers or reboot before relying on the new permissions.

togi runs on NixOS. Development works on Linux and on macOS (aarch64-darwin). On macOS, the dev shell, the tests, `just sim`, every flake check except the VM tests and the trial scope tests, and `status`, `events` and `reset` against a copied state directory work; CI runs the Linux-only checks, and `togi run` exits with an error.
