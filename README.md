# togi

> [!WARNING]
> togi is pre-1.0. It writes Curve Optimizer offsets to your CPU through `ryzen_smu`, and mapping where each CCD fails means running it until it fails: expect crashes, reboots and lost work in anything else running. A new version can refuse to continue a session written by an older one. Run it at your own risk.

Moves a Zen 5 desktop CPU towards deeper per-CCD Curve Optimizer offsets, as far as its own test evidence supports: nights test, days observe. A night runs until you end it (Ctrl-C, F10 in the dashboard, or a reboot); a crash reboot continues it, and the next run after you end it opens the next night.

- Nights [map](docs/spec/tuner.md#night-edge-u) each CCD's failure edge alone, derive each CCD's [cap](docs/spec/tuner.md#cap-s-margin-from-measured-slope) from the measured failure rate, run [CO-0 controls](docs/spec/tuner.md#co-0-controls) and qualify the exact next day profile across the whole [coverage matrix](docs/spec/workloads.md#coverage-matrix).
- Days apply the committed permitted [Ruleset 14 profile D](docs/spec/tuner-ruleset14.md#4-a-clean-hold-earns-one-step), with exact-profile qualification, clean holds and [event retreats](docs/spec/tuner.md#whole-profile-veto-and-escalation). Nonzero profiles remain bounded experiments, not reliability guarantees.
- Tests with self-checking workloads (mprime, y-cruncher) across light, heavy, load-step, medium, SMT, idle and all-core regimes.
- Survives crashes: the next run reads its journal, attributes the crash and continues.
- Records every action in a plain-text journal you can read to see what it did and why.
- BIOS stays at CO 0, and a nonzero starting baseline is refused. The day service applies the permitted D at normal boot and reapplies it without promotion on restart or resume, with an armed, counting PID 1 watchdog and verified readback ([day transitions](docs/spec/runtime.md#ruleset-13-policy-boundary)).

*togi* (研ぎ) is Japanese for polishing a blade: coarse stones first, then fine ones, until the edge shows.

## Status

Works today, on a simulated 16-core machine:
- Ruleset 14 two-ends D: isolated night edge U, hard ceiling C, exact-profile qualification and 50-hour holds (including stock) for bounded day steps; newest-step stops, whole-profile vetoes and safety retreats;
- a Ruleset 13 successor archived unchanged and restarted at verified stock, importing raw daily and compatible night observations and reconstructing campaign events/stops/vetoes ([transition](docs/spec/tuner-ruleset14.md#10-journal-schema-and-architectural-record));
- a durable two-event machine allowance, including provisional charges, never renewed by restart, reset, firmware change, new session or reboot; missing campaign history blocks nonzero exploration;
- status/watch D/C/U, qualification, hold and finish reasons, allowance and experimental evidence (`experimental: N clean hours; at most 1 crash per X h (95%, fixed-rate model)`); S is diagnostic only, never permission;
- console-compatible dashboard rendering and a [rich truecolour profile](docs/spec/runtime.md#dashboard) using the same colours with Unicode gauges and text;
- normal-boot profile application and observation with `togi day`: durable intent/write/all-core readback, zero-before-clean-stop ordering, checkpointed hours, clean/unclean ends and questions you answer without root. Answers and evidence classify [daily events](docs/spec/tuner.md#daily-events).

Built for real hardware, a Granite Ridge desktop running NixOS, and tested piece by piece on one with GRUB:
- the `ryzen_smu` driver on full 8-core CCDs only; CCDs with fused-off slots are not yet supported;
- mprime and y-cruncher trials, each confined to its cores in a systemd scope;
- machine-check detection from the kernel log;
- the NixOS module and unattended tuning boot support GRUB and systemd-boot, with two service restart-limit retry boots before returning to the normal system and durable leave reasons; VM checks cover switching between day and night without tuning hardware.

Not yet:
- a full night-and-day cycle on real hardware.

`just sim` runs a simulated session from a source checkout, for development; see [docs/simulating.md](docs/simulating.md).

## Usage

```sh
togi --version                         # version and git revision of this build
sudo togi doctor                       # check this machine is ready for a run, without starting a session or writing the journal
sudo togi doctor --day                 # check day prerequisites and this boot's recorded stock-baseline proof
sudo togi run                          # tune tonight; Ctrl-C ends the night, the next run opens the next
sudo togi night                        # GRUB or systemd-boot: confirm, remember the day's selection and reboot into tuning
sudo togi day                          # apply permitted D and observe; needs verified CO 0 and PID 1's counting watchdog
togi day answer <boot> frozen          # answer what an unclean end was: frozen, graphics, power-off or unsure
sudo togi day record                   # record a missed crash through the live service; no stop needed
togi day pause                         # verify stock until the next cold boot
togi day pause 3h                      # or use --until 18:30; timed pauses survive restart and reboot
togi day resume                        # end the pause and verify the still-permitted committed D
togi day freeze                        # stop promotions; use unfreeze to lift it, safety retreats still run
togi status                            # activity, each CCD's D, S and U, night queues, day policy and observations
togi status --evidence                 # cap reasons, hours, coverage gaps and profile/stock evidence; no verdict
togi watch                             # full-width dashboard: current trial, every core
togi --state-dir <dir> status          # inspect a copied journal
togi --state-dir <dir> events --core 3 # everything that happened to core 3
source <(togi completion bash)         # shell completions; the Nix package installs bash, zsh, fish and nushell ones
```

`doctor` and `run` read JSON configuration from `/etc/togi/config.json`; `--config FILE` selects another file. The NixOS module generates it from `services.togi.settings`. See [Configuration](docs/spec/runtime.md#configuration) for keys, defaults and a hand-written example.

The dashboard adapts to wide and compact terminals. It shows the stage line, one table per CCD with its D, S and U, and the night and day panels beside recent decisions; `?` explains the gauges and `l` opens the journal. Outcome lines come from the tuner itself. [Commands](docs/spec/runtime.md#commands) describes when the screen redraws.

`status --evidence` explains the current [ceiling and qualification rules](docs/spec/tuner-ruleset14.md#2-night-edge-and-hard-ceiling), including why a profile remains at zero. Recorded hours and incomplete coverage are evidence, not a promise of another step or a completion estimate. On the first observed day boot after a night, `status` and `togi day notify` report the night's recorded work and the verified applied profile, not merely a proposal.

With the day service running and your user in `services.togi.day.users`, pause/resume and freeze/unfreeze need no root shell. On the main `watch` view, `p` toggles pause and `f` toggles freeze. Timed pauses warn five minutes before expiry and offer a one-hour extension. Expiry and early resume reapply the still-permitted committed D without promotion; [pause and freeze](docs/howto.md#pausing-for-important-work) explain the lifetimes.

`status` adds a Day section after observations exist: observed boots, checkpointed hours per read-back offset profile, clean and unclean ends, and the latest unclean boot's reset reason. Each unclean end becomes a question that `status` and `watch` show, with what togi saw, until you answer it with `togi day answer <boot> <frozen|graphics|power-off|unsure>`; no root shell is needed. The answer counts: `frozen` makes the end CPU-suspect, which vetoes that profile and retreats the day step, and a later answer can reclassify it ([Daily events](docs/spec/tuner.md#daily-events)). `togi day notify` raises desktop notifications through the installed `notify-send`, with answer buttons on KDE Plasma, pause-expiry warnings and verified profile-reapplication notices. See [day observation](docs/howto.md#daily-observation) for service setup and [answering](docs/howto.md#answering-what-happened).

The NixOS tuning specialisation supports exactly one of GRUB or systemd-boot. `sudo togi night` selects it persistently, so crash reboots stay in the night; F10 on its dashboard or the power button restores the previous boot selection. See [Night and day](docs/spec/runtime.md#night-and-day) for saved defaults and menu-started nights.

[docs/howto.md](docs/howto.md) walks through installing the NixOS module, a first in-session run and an overnight tuning boot. `togi --help` lists every command, and `togi <command> --help` gives its description, examples and flags. [Commands](docs/spec/runtime.md#commands) describes each one in full.

## Documentation

|Document|Read it for|
|---|---|
|[docs/howto.md](docs/howto.md)|Installing togi and running a tuning session|
|[docs/how-togi-tunes.md](docs/how-togi-tunes.md)|Two-ends tuning: nights and days, D/C/U, holds, campaign allowance and history|
|[docs/releasing.md](docs/releasing.md)|Versioning, the release workflow and tags|
|[docs/simulating.md](docs/simulating.md)|Running a simulated session for development|
|[docs/reviewing.md](docs/reviewing.md)|Reviewing current and archived runs with `just stats`, and writing a run's retro|
|[docs/benchmarking.md](docs/benchmarking.md)|Comparing tuner changes across simulated machines|
|[CHANGELOG.md](CHANGELOG.md)|What changed, newest first|
|[GLOSSARY.md](GLOSSARY.md)|The vocabulary: offsets, profiles, nights and days, D, S and U|
|[docs/spec/tuner-ruleset14.md](docs/spec/tuner-ruleset14.md)|The normative Ruleset 14 policy; [Ruleset 13 history and retained contracts](docs/spec/tuner.md)|
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

Enter the dev shell with `nix develop`, or run `direnv allow` once if you use direnv; recipes that run cargo or `gh` stop with that hint outside it. `just test` is the tight loop. `just gate` checks formatting, lint, the other supported platform through clippy, the NixOS module's evaluation, changelog fragments, local nextest checks, doc tests, cargo-deny's bans, licenses and sources, cargo-shear, and the board, coverage-selection and SQL fixtures. `just push-fix` runs the review-fix checks. These commands and `just check` skip CI-tier checks on every host; `just vm [names]` opts into VM builds on Linux with writable KVM. PR CI gates landing on `main` and the release dispatches every Linux flake check, including the CI tier ([check tiers](docs/development.md#shell-and-checks)). `just check-main` dispatches and follows that full run; `just check` builds every local-tier flake check, including `package` and cargo-deny's advisory audit beyond the gate. Run `just` to list recipes; [Development](docs/development.md#shell-and-checks) owns the check ladder and tiers.

Simulations are on-demand research tools, not checks: use `just sim-suite`, `just same`, `just smoke` or the benches ([research policy](docs/development.md#simulations-and-research)).

The coordination board (`just board`) is Nushell. `just board-test` runs its fixed-fixture assertions without GitHub access; the `quality` flake check runs them in both `just gate` and `just push-fix`.

CI pushes each check's output to the public Cachix cache `togi`, and local `just check` can download those instead of building them. Nix uses the cache only when the machine's own configuration trusts it, so opting in is a choice per machine: either add the cache to the system Nix configuration (`/etc/nix/nix.conf`, `/etc/nix/nix.custom.conf` under Determinate Nix, or the same two settings under `nix.settings` on NixOS or nix-darwin),

```
extra-substituters = https://togi.cachix.org
extra-trusted-public-keys = togi.cachix.org-1:1EZ2zQlDkNhHZmROZzR0n0/CcYGLPfAmPs+VaGL72LU=
```

or run `cachix use togi` as a user listed in `trusted-users`. After changing the system configuration file, by hand or by running `cachix use togi` as root outside NixOS, restart the Nix daemon, because it reads that file only when it starts. Local `just check` then downloads every local-tier check whose inputs are unchanged and builds only the rest. Trusting the key makes Nix on that machine accept any store path the cache serves, for any build and not only togi's checks, including outputs pushed by pull request runs that hold the push token ([cache trust](docs/development.md#check-cache)). The flake configures no cache ([cache policy](docs/development.md#check-cache)).

Hardware tests share the [host lock](docs/spec/runtime.md#host-lock) with `run` and `reset`. Delegated users need lock access as well as SMU and cpuset-controller permissions; see [host-lock provisioning](docs/howto.md#host-lock-and-delegated-hardware-tests). After upgrading from a public-readable lock, quiesce old lock openers or reboot before relying on the new permissions.

togi runs on NixOS. Development works on Linux and on macOS (aarch64-darwin). On macOS, the dev shell, the tests, `just sim`, every flake check except the VM tests and the trial scope tests, and `status`, `events` and `reset` against a copied state directory work; CI runs only on Linux and builds nothing on macOS ([CI policy](docs/development.md#ci-and-release-gates)), and `togi run` exits with an error.
