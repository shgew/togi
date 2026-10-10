# togi

> [!WARNING]
> togi is pre-1.0. It writes Curve Optimizer offsets to your CPU through `ryzen_smu`, and finding each core's solo limit means running it until it fails: expect crashes, reboots and lost work in anything else running. A new version can refuse to continue a session written by an older one. Run it at your own risk.

Finds the deepest per-core Curve Optimizer offsets a Zen 5 desktop CPU sustains, then keeps testing them through full checking cycles. Choose how long to keep testing with `run --cycles N`, or let checking continue indefinitely.

- [Searches](docs/spec/tuner.md#search) each core alone, makes multi-core R7 cores [self-sufficient](docs/spec/tuner.md#r7-request-order-and-attribution), [hunts](docs/spec/tuner.md#hunt) unattributed failures outside multi-core R7 and on the idle cores of a failed multi-core R7 load, starts [checking](docs/spec/tuner.md#phase-1) one count shallower than each solo limit, [takes that margin back](docs/spec/tuner.md#phase-2) in a bounded phase 2 and confirms it before continuing [checking](docs/spec/tuner.md#checking).
- Tests with self-checking workloads (mprime, y-cruncher) across light, heavy, load-step, medium, SMT, idle and all-core regimes.
- Survives crashes: the next run reads its journal, attributes the crash and continues.
- Records every action in a plain-text journal you can read to see what it did and why.
- Reports the offsets; you enter them in BIOS.

*togi* (研ぎ) is Japanese for polishing a blade: coarse stones first, then fine ones, until the edge shows.

## Status

Works today, on a simulated 16-core machine:
- the full simulated tuning lifecycle: per-core search, failure hunts, combinations, the two tuning phases, full checking cycles, crash resume and reset;
- a seeded session after a ruleset update or BIOS change, carrying what [Transitions](docs/spec/journal.md#transitions) and [Fact eligibility](docs/spec/journal.md#fact-eligibility) allow;
- evidence-based hunt duration and singleton-probe scheduling ([Hunt](docs/spec/tuner.md#hunt));
- R7 full parts and partial chains ([Together trial sequence](docs/spec/tuner.md#together-trial-sequence)), with [voltage-targeted backoff](docs/spec/tuner.md#r7-voltage-targeted-backoff) of multi-core R7 failures and [located hunts](docs/spec/tuner.md#hunt) of unattributed ones;
- an all-zero rerun before a failure at CO 0 stops tuning ([Dead ends](docs/spec/tuner.md#dead-ends));
- reading a session's hunt, combinations, clean cycles, top requesters, per-workload self-sufficiency, valid trials and Tctl peak with `status`, `events` and the live `watch` dashboard ([Commands](docs/spec/runtime.md#commands)).

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
sudo togi doctor                       # check this machine is ready for a run, without starting a session or writing the journal
sudo togi run                          # tune this machine; Ctrl-C stops, the next run resumes
sudo togi run --cycles 3            # stop after three clean cycles, counted from phase 2's confirmation
togi status                            # activity, failure points and combinations, per-core offsets and evidence
togi watch                             # full-width dashboard: current trial, tuner forecasts, every core
togi --state-dir <dir> status          # inspect a copied journal
togi --state-dir <dir> events --core 3 # everything that happened to core 3
source <(togi completion bash)         # shell completions; the Nix package installs bash, zsh, fish and nushell ones
```

`doctor`, `run` and `reset` read JSON configuration from `/etc/togi/config.json`; `--config FILE` selects another file. The NixOS module generates it from `services.togi.settings`. See [Configuration](docs/spec/runtime.md#configuration) for keys, defaults and a hand-written example.

The dashboard adapts to wide and compact terminals. It shows checking's cycle checklist, hunt parts and member probes, or search turns beside recent decisions; `?` explains the gauges and `l` opens the journal. Outcome lines come from the tuner itself. [Commands](docs/spec/runtime.md#commands) describes when the screen redraws.

[docs/howto.md](docs/howto.md) walks through installing the NixOS module, a first in-session run and an overnight tuning boot. `togi --help` lists every command, and `togi <command> --help` gives its description, examples and flags. [Commands](docs/spec/runtime.md#commands) describes each one in full.

## Documentation

|Document|Read it for|
|---|---|
|[docs/howto.md](docs/howto.md)|Installing togi and running a tuning session|
|[docs/how-togi-tunes.md](docs/how-togi-tunes.md)|The tuning sequence, phase costs and ruleset history|
|[docs/releasing.md](docs/releasing.md)|Versioning, the release workflow and tags|
|[docs/simulating.md](docs/simulating.md)|Running a simulated session for development|
|[docs/reviewing.md](docs/reviewing.md)|Reviewing current and archived runs with `just stats`, scoring a run against its forecast, and writing a run's retro|
|[docs/benchmarking.md](docs/benchmarking.md)|Comparing tuner changes across simulated machines, and forecasting a real run|
|[CHANGELOG.md](CHANGELOG.md)|What changed, newest first|
|[GLOSSARY.md](GLOSSARY.md)|The vocabulary: offsets, phases, regimes, clean cycles|
|[docs/spec/tuner.md](docs/spec/tuner.md)|How offsets are searched, hunted and checked, in two phases|
|[docs/spec/workloads.md](docs/spec/workloads.md)|The workload regimes and how failures are detected|
|[docs/spec/journal.md](docs/spec/journal.md)|The journal, its events and the state file|
|[docs/spec/runtime.md](docs/spec/runtime.md)|Commands, configuration, the tuning boot and the NixOS module|
|[docs/adr/](docs/adr/)|Why each major decision was made|
|[docs/prior-art.md](docs/prior-art.md)|What was taken from, and left out of, earlier tools|
|[Issues](https://github.com/shgew/togi/issues)|The plan, ideas and bugs; the `1.0` milestone holds what ships in 1.0|
|[CONTRIBUTING.md](CONTRIBUTING.md)|Why togi takes no contributions|
|[AGENTS.md](AGENTS.md)|Working on togi: workflow, commands and conventions|

## Contributing

togi takes no contributions: issues, comments and pull requests are limited to collaborators. [CONTRIBUTING.md](CONTRIBUTING.md) has the details.

## License

Copyright (C) 2026 Hleb Shauchenka. togi is free software under the [GNU General Public License, version 3 or any later version](LICENSE) (GPL-3.0-or-later). Releases up to and including 0.9.0 were published under the MIT license and remain available under it ([ADR 0035](docs/adr/0035-gpl-3.0-or-later.md)).

## Development

Enter the dev shell with `nix develop`, or run `direnv allow` once if you use direnv; recipes that run Go stop with that hint outside it. `just test` is the tight loop, and `just gate` runs every non-VM flake check sequentially, using warm Go caches: formatting, lint, the NixOS module's evaluation, changelog fragments, the vendored Go modules, shuffled integration-tagged tests, the race detector and, on Linux, the hardware-tagged trial test compile. `just check` adds the Linux VM tests and stays CI's definition of green; CI runs every check on every pull request. Run `just` to list the recipes. [AGENTS.md](AGENTS.md) has the agent check ladder and the rest.

CI pushes each check's output to the public Cachix cache `togi`, and local `just check` can download those instead of building them. Nix uses the cache only when the machine's own configuration trusts it, so opting in is a choice per machine: either add the cache to the system Nix configuration (`/etc/nix/nix.conf`, `/etc/nix/nix.custom.conf` under Determinate Nix, or the same two settings under `nix.settings` on NixOS or nix-darwin),

```
extra-substituters = https://togi.cachix.org
extra-trusted-public-keys = togi.cachix.org-1:1EZ2zQlDkNhHZmROZzR0n0/CcYGLPfAmPs+VaGL72LU=
```

or run `cachix use togi` as a user listed in `trusted-users`. After changing the system configuration file, by hand or by running `cachix use togi` as root outside NixOS, restart the Nix daemon, because it reads that file only when it starts. Local `just check` then downloads every check whose inputs are unchanged and builds only the rest. Trusting the key makes Nix on that machine accept any store path the cache serves, for any build and not only togi's checks, including outputs pushed by pull request runs that hold the push token ([ADR 0021](docs/adr/0021-cache-check-outputs-on-cachix.md)). The flake configures no cache ([ADR 0042](docs/adr/0042-opt-in-to-the-check-cache-locally.md)).

Hardware tests share the [host lock](docs/spec/runtime.md#host-lock) with `run` and `reset`. Delegated users need lock access as well as SMU and cpuset-controller permissions; see [host-lock provisioning](docs/howto.md#host-lock-and-delegated-hardware-tests). After upgrading from a public-readable lock, quiesce old lock openers or reboot before relying on the new permissions.

togi runs on NixOS. Development works on Linux and on macOS (aarch64-darwin). On macOS, the dev shell, the tests, `just sim`, every flake check except the VM tests and the trial scope tests, and `status`, `events` and `reset` against a copied state directory work; CI runs the Linux-only checks, and `togi run` exits with an error.
