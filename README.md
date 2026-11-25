# togi

> [!WARNING]
> togi is experimental and pre-1.0. It writes Curve Optimizer offsets to your CPU through `ryzen_smu` and deliberately searches for failures. Expect crashes, reboots and lost work. A new build can refuse to continue an older session. Run it at your own risk.

The goal is automatic per-core Curve Optimizer tuning. Today, togi tunes per CCD on Zen 5 desktops: nights test, days apply and observe. [How togi tunes](docs/how-togi-tunes.md) describes the shipped policy; the [edge–margin–wait model](docs/model.md) is the design being built.

Nights map each CCD in isolation and test proposed day profiles with mprime and y-cruncher. Days apply the permitted profile and record observations in the journal. Test passes and recorded clean hours are evidence under the conditions observed, not a reliability guarantee, a forecast, or proof that desktop freezes are covered.

*togi* (研ぎ) is Japanese for polishing a blade: coarse stones first, then fine ones, until the edge shows.

## Status

The NixOS runtime supports GRUB and full eight-core Granite Ridge CCDs. Components have been exercised on real hardware, and VM checks exercise boot and application mechanisms without tuning hardware. A complete night-and-day tuning cycle on real hardware has not been recorded.

`just sim` runs a simulated session from a source checkout, for development; see [docs/simulating.md](docs/simulating.md).

## Usage

```sh
togi --version                         # version and git revision of this build
sudo togi doctor                       # check this machine is ready for a run, without starting a session or writing the journal
sudo togi doctor --day                 # check day prerequisites and a current, verified stock readback recorded in this boot
sudo togi run                          # foreground tuning with event lines; needs current stock attestation from `togi night`
sudo togi night                        # GRUB: confirm stock stability, remember the day's selection and reboot into tuning
sudo togi day                          # apply permitted D and observe; needs verified CO 0 and PID 1's counting watchdog
togi day answer <boot> frozen          # answer what an unclean end was: frozen, graphics, power-off or unsure
togi day pause                         # verify stock until the next cold boot
togi day pause 3h                      # or use --until 18:30; timed pauses survive restart and reboot
togi day resume                        # end the pause and verify the still-permitted committed D
togi day freeze                        # stop promotions; use unfreeze to lift it, safety retreats still run
togi status                            # activity, each CCD's D, C and U, night queues and day observations
togi status --evidence                 # exact-profile evidence, outstanding coverage and profile history
togi watch                             # Linux live dashboard: current trial, every core
togi --state-dir <dir> status          # inspect a copied journal
togi --state-dir <dir> watch > frame.txt # render one frame from a copied journal
source <(togi completion bash)         # shell completions; the Nix package installs bash, zsh, fish and nushell ones
```

`doctor` and `run` read JSON configuration from `/etc/togi/config.json`; `--config FILE` selects another file. The NixOS module generates it from `services.togi.settings`. See [Configuration](docs/spec/runtime.md#configuration) for keys, defaults and a hand-written example.

`watch` owns the dashboard; `run` only logs events and final diagnostics. See [morning status and evidence](docs/howto.md#5-in-the-morning), [daily observation and crash questions](docs/howto.md#daily-observation), and [pause and freeze](docs/howto.md#pausing-for-important-work).

The NixOS tuning specialisation requires GRUB. `sudo togi night` selects it persistently, so crash reboots stay in the night; F10 on its dashboard or the power button restores the previous boot selection. See [Night and day](docs/spec/runtime.md#night-and-day) for saved defaults and menu-started nights.

[docs/howto.md](docs/howto.md) walks through installing the NixOS module, a first in-session run and an overnight tuning boot. `togi --help` lists operator-facing commands, and `togi <command> --help` gives its description, examples and flags. [Commands](docs/spec/runtime.md#commands) describes each one in full.

## Documentation

|Document|Read it for|
|---|---|
|[docs/howto.md](docs/howto.md)|Installing togi and running a tuning session|
|[docs/how-togi-tunes.md](docs/how-togi-tunes.md)|Two-ends tuning: nights and days, D/C/U, holds, campaign allowance and history|
|[docs/development.md](docs/development.md#releases-and-changelog)|Pre-1.0 updates and build identification|
|[docs/simulating.md](docs/simulating.md)|Running a simulated session for development|
|[docs/reviewing.md](docs/reviewing.md)|Reviewing current and archived runs with `just stats`, and writing a run's retro|
|[docs/benchmarking.md](docs/benchmarking.md)|Comparing tuner changes in simulation, not measuring hardware reliability|
|[CHANGELOG.md](CHANGELOG.md)|What changed, newest first|
|[GLOSSARY.md](GLOSSARY.md)|The vocabulary: offsets, profiles, nights and days, D, C and U|
|[docs/spec/tuner-ruleset14.md](docs/spec/tuner-ruleset14.md)|The normative Ruleset 14 policy; [Ruleset 13 history and retained contracts](docs/spec/tuner.md)|
|[docs/spec/workloads.md](docs/spec/workloads.md)|The workload regimes and how failures are detected|
|[docs/spec/journal.md](docs/spec/journal.md)|The journal and its events|
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

See [Development](docs/development.md) for the dev shell, check ladder, CI and on-demand simulations.

CI pushes each check's output to the public Cachix cache `togi`, and local `just check` can download those instead of building them. Nix uses the cache only when the machine's own configuration trusts it, so opting in is a choice per machine: either add the cache to the system Nix configuration (`/etc/nix/nix.conf`, `/etc/nix/nix.custom.conf` under Determinate Nix, or the same two settings under `nix.settings` on NixOS or nix-darwin),

```
extra-substituters = https://togi.cachix.org
extra-trusted-public-keys = togi.cachix.org-1:1EZ2zQlDkNhHZmROZzR0n0/CcYGLPfAmPs+VaGL72LU=
```

or run `cachix use togi` as a user listed in `trusted-users`. After changing the system configuration file, by hand or by running `cachix use togi` as root outside NixOS, restart the Nix daemon, because it reads that file only when it starts. Local `just check` then downloads every local-tier check whose inputs are unchanged and builds only the rest. Trusting the key makes Nix on that machine accept any store path the cache serves, for any build and not only togi's checks, including outputs pushed by pull request runs that hold the push token ([cache trust](docs/development.md#check-cache)). The flake configures no cache ([cache policy](docs/development.md#check-cache)).

Hardware tests run as root and share the [host lock](docs/spec/runtime.md#host-lock) that the NixOS module creates for `run` and `reset`.

togi runs on NixOS. Development works on Linux and on macOS (aarch64-darwin). On macOS you can build, test, simulate, replay recordings, and read a copied journal with `status` or a redirected `watch`. Hardware runs and live watching require Linux. [CI runs only on Linux](docs/development.md#ci-and-release-gates).
