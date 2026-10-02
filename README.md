# togi

> [!WARNING]
> togi is pre-1.0. It writes Curve Optimizer offsets to your CPU through `ryzen_smu`, and finding each core's edge means running it until it fails: expect crashes, reboots and lost work in anything else running. A new version can refuse to continue a session written by an older one. Run it at your own risk.

Finds the deepest per-core Curve Optimizer offsets a Zen 5 desktop CPU sustains, then keeps testing them so the result earns a durability tier: Bronze, Silver or Gold today. Platinum requires the planned `observe` service and cannot be earned yet.

- Searches each core in isolation, hunts the cores behind unattributed failures, then refines the resident profile to the most total depth its marks allow before continuing guard.
- Tests with self-checking workloads (mprime, y-cruncher) across light, heavy, load-step, medium, SMT, idle and all-core regimes.
- Survives crashes: the next run reads its journal, attributes the crash and continues.
- Records every action in a plain-text journal you can read to see what it did and why.
- Reports the offsets; you enter them in BIOS.

*togi* (研ぎ) is Japanese for polishing a blade: coarse stones first, then fine ones, until the edge shows.

## Status

Works today, on a simulated 16-core machine:
- the full simulated tuning lifecycle: per-core search, failure hunts, joint marks, resident refinement, qualifying guard rotations, crash resume, tiers and reset;
- a seeded session after a ruleset update or BIOS change: a ruleset change carries eligible same-BIOS trial facts for candidate-edge checks, hunt masks, reruns and refinement, while rotations qualify only on live passes; a BIOS change carries edges but not failed marks or trial facts;
- evidence-based hunt duration and singleton-probe scheduling, and credit for an earlier uncontradicted qualifying rotation at an equal or deeper profile, without carrying earlier clean hours into Silver or Gold;
- reading a session's hunt, joint marks and per-workload exposure with `status`, `events` and the live `watch` dashboard.

Built for real hardware, a Granite Ridge desktop running NixOS with GRUB, and tested piece by piece on one:
- the `ryzen_smu` driver on full 8-core CCDs only; CCDs with fused-off slots are not yet supported;
- mprime and y-cruncher trials, each confined to its cores in a systemd scope;
- machine-check detection from the kernel log;
- the NixOS module and the unattended tuning boot, checked in a NixOS VM.

Not yet:
- a full tuning session on real hardware, start to Bronze.

`just sim` runs a simulated session from a source checkout, for development; see [docs/simulating.md](docs/simulating.md).

## Usage

```sh
togi --version                         # version and git revision of this build
sudo togi run                          # tune this machine; Ctrl-C stops, the next run resumes
togi status                            # activity, marks, per-core offsets and evidence
togi watch                             # live hunt, refinement and guard dashboard
togi --state-dir <dir> status          # inspect a copied journal
togi --state-dir <dir> events --core 3 # everything that happened to core 3
```

[docs/howto.md](docs/howto.md) walks through installing the NixOS module, a first in-session run and an overnight tuning boot. `togi --help` lists every command, and `togi <command> --help` gives its description, examples and flags. [Commands](docs/spec/runtime.md#commands) describes each one in full.

## Documentation

|Document|Read it for|
|---|---|
|[docs/howto.md](docs/howto.md)|Installing togi and running a tuning session|
|[docs/releasing.md](docs/releasing.md)|Versioning, the release workflow and tags|
|[docs/simulating.md](docs/simulating.md)|Running a simulated session for development|
|[docs/reviewing.md](docs/reviewing.md)|Reviewing current and archived runs with `just stats`|
|[docs/benchmarking.md](docs/benchmarking.md)|Comparing tuner changes across simulated machines|
|[CHANGELOG.md](CHANGELOG.md)|What changed, newest first|
|[CONTEXT.md](CONTEXT.md)|The vocabulary: offsets, phases, regimes, tiers|
|[docs/spec/tuner.md](docs/spec/tuner.md)|How offsets are searched, hunted, refined and guarded, and how tiers are earned|
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

[MIT](LICENSE).

## Development

Enter the dev shell with `nix develop`, or run `direnv allow` once if you use direnv. `just test` is the tight loop, and `just gate` runs lint, the formatting check and tests. `just check` runs every flake check: the package with its tests, lint, formatting and the NixOS VM tests; CI runs them on every pull request. Recipes also work outside the dev shell; run `just` to list them. [AGENTS.md](AGENTS.md) has the rest.

Hardware tests share a private host lock with `run` and `reset`. Delegated users need explicit lock access as well as SMU and cpuset-controller permissions; see [host-lock provisioning](docs/howto.md#host-lock-and-delegated-hardware-tests). After upgrading from a public-readable lock, quiesce old lock openers or reboot before relying on the new permissions.

togi runs on NixOS. Development also works on macOS (aarch64-darwin): the dev shell, the tests, `just sim`, and `status`, `events` and `reset` against a copied state directory. `just check` there skips the VM tests, and `togi run` exits with an error.
