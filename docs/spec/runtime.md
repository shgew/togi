# Runtime

Normative rules for how shycler is invoked, configured and deployed.

## Commands

| Command | Writes journal | Purpose |
|---|---|---|
| `shycler run [--sim <seed>] [--rotations <N>] [--tuning-boot <grubenv>]` | yes | Start or resume the session in the foreground. The same entry point serves in-session runs and the tuning boot service, which passes `--tuning-boot` with its GRUB environment file (Dead-end actions). With `--rotations N` (N >= 1) it stops, recording `shutdown`, when a rotation would start after the current profile survived N clean guard rotations; without it guard is endless |
| `shycler status` | no | Session and BIOS context; phase, tier and guard progress; in-flight action and dead end; per-core table of offset, phase, failed mark, unproven depth, queued command and last decision; clean hours and failure-rate bound overall and per regime, and the highest Tctl. Rendered from a replay of the journal, not `state.json` |
| `shycler cert` | no | Render the certificate (`tuner.md`) from a replay of the journal, with the SHA-256 of the lines it rendered |
| `shycler events` | no | Render the journal with filters (`journal.md`) |
| `shycler regain [--core <N>]` | yes | Queue one count of regain on every confirmed core with unproven depth, or on core N only (`tuner.md`). Prints `regain queued for cores 03, 07; the next shycler run confirms one count deeper on each`. Exit 1 when a regain is already queued or running, or nothing is left to regain |
| `shycler reset --core <N> \| --all` | yes | Exactly one of the two, else exit 2. `--core N` queues the core's reset and prints `reset of core 03 queued; the next shycler run restarts its search from the baseline`. `--all` archives the session and prints `session <id> archived to archive/<id>.jsonl; the next shycler run starts a new session` |

`regain` and `reset` read the journal before they open it: no journal or no session exits 1 without creating anything. A core that is not in the session exits 2, and a `run` holding the lock exits 3. Both record their events in the host's current boot, and end it with `shutdown`, so the next `run` treats that boot as ended cleanly.

Global flags: `--config <path>` (default `/etc/shycler/config.toml`), `--state-dir <path>` (default `/var/lib/shycler`).

`shycler` without a command prints the wordmark and the tagline `per-core Curve Optimizer` above the usage, to stderr, and exits 2. `--help`, an unknown command and a flag error print the usage alone. The usage holds the synopsis, a description, examples, the commands and the global flags.

`shycler <command> --help` prints the command's synopsis, a description of what it does, one or two examples, its own flags and then the global flags, to stdout, and exits 0. A flag error prints the error and the same help to stderr and exits 2. Every flag is shown in its `--long` form.

`shycler run --sim <seed>` drives the seeded simulator (`internal/sim`) instead of hardware: a 16-core machine whose crash reboots are handled in-process, with a real journal. It uses `--state-dir` when given, else a new temporary directory whose path it prints to stderr, and it fsyncs nothing. With `--sim`, `--rotations` defaults to 1. A state directory that already holds a journal or archives resumes the simulated machine after them: boot numbering continues and the clock starts after the last event, so a crash in the new run is never mistaken for an old boot, and a session after `reset --all` gets a new id.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Stopped cleanly: by a signal, or after the requested clean rotations |
| 1 | Error |
| 2 | Usage or configuration error, including a start offset for a core that does not exist |
| 3 | The journal is held by another writer |
| 10 | Dead end `failure_at_zero` |
| 11 | Dead end `smu` |
| 12 | Dead end `no_evidence` |
| 13 | Dead end `boot_loop` |
| 14 | Dead end `containment` |
| 15 | Dead end `preflight` |

`run` records `shutdown` only for the clean stops (exit 0) and the dead ends (10-15). Any other exit writes nothing, so the next boot treats the gap as a crash.

## Privileges

`run`, `regain` and `reset` require root: they change core voltage and the boot entry, and create cgroup scopes. The read-only commands work for any user who can read the state directory.

## Preflight

`run` checks, each recorded as a `preflight.check` event:
1. Running as root.
2. The CPU is family `0x1A`, model `0x40`-`0x4F` (Granite Ridge desktop).
3. `ryzen_smu` is loaded and reports a matching codename.
4. Every core's offset reads back through the SMU.
5. The core-to-SMU slot mapping is verified (`../prior-art.md`).
6. Both backends are configured and present.
7. `systemd-run` can create a scope confined to CPU 0.
8. The BIOS context matches the session, when resuming and checks 1-7 passed.

Any failed check is a dead end.

## Configuration

TOML at the `--config` path, produced by the NixOS module from `services.shycler.settings`:

| Key | Default | Valid |
|---|---|---|
| `start_offsets.<core>` | none | Offset override per core id, within [-50, 0] |
| `durations.search_trial_s` | 90 | [1, 86400] |
| `durations.confirmation_trial_s` | 300 | [1, 86400] |
| `durations.guard_trial_s` | 120 | [1, 86400] |
| `durations.guard_idle_s` | 900 | [1, 86400]; R6 |
| `durations.guard_all_core_s` | 1200 | [1, 86400]; R7, half on both CCDs and a quarter per CCD |
| `guard.rotation` | `["R1", "R2", "R6", "R3", "R4", "R7", "R5", "R6"]` | Non-empty list of regimes |
| `dead_ends.inconclusive_in_a_row` | 3 | [1, 100] |
| `dead_ends.stray_crashes_in_a_row` | 3 | [1, 100] |
| `backends.mprime` | not configured | Absolute path of the package (`bin/mprime` inside it) |
| `backends.ycruncher` | not configured | Absolute path of the package (`lib/y-cruncher/Binaries` inside it) |

Unknown keys and out-of-range values are errors. When `--config` is not given and no file exists at the default path, the defaults apply.

The effective configuration is recorded in `config.loaded` at every start. Configuration that changes the meaning of existing evidence, such as trial durations, is allowed mid-session and takes effect from the next trial. The journal shows when it changed.

## In-session run

`sudo shycler run` on a normal boot. If the machine crashes, it comes back to the normal desktop. The next `sudo shycler run` detects the crash from the journal, attributes it to the in-flight action, and continues. Nothing restarts shycler automatically outside a tuning boot.

## Tuning boot

`services.shycler.tuning.enable` adds a GRUB specialisation named `shycler` and makes GRUB remember the last booted entry (`boot.loader.grub.default = "saved"`). Picking "shycler" once in the menu starts unattended tuning. Every crash reboot returns to it until a dead end, or you, select a normal entry. The module asserts that GRUB is the bootloader; other bootloaders are on the roadmap.

Inside the specialisation:
- `systemd.defaultUnit = "multi-user.target"`, so no graphical session starts;
- `shycler.service` runs `shycler run --tuning-boot <grubenv>` as root, where `<grubenv>` is `grub/grubenv` under the first of `boot.loader.grub.mirroredBoots` (normally `/boot/grub/grubenv`), with `Restart=on-failure` and `RestartSec=60`, and a start limit of 3 per 30 minutes. `RestartPreventExitStatus` lists the dead-end exit codes 10-15;
- a tty1 unit follows `shycler.service`'s log, in place of the tty1 login prompt;
- sysctls `kernel.panic=10`, `kernel.panic_on_oops=1`, `kernel.hardlockup_panic=1`, `kernel.softlockup_panic=1`;
- `systemd.settings.Manager.RuntimeWatchdogSec = "30s"`, so the SP5100 TCO hardware watchdog resets a frozen machine;
- journald `Storage=persistent` and `SyncIntervalSec=1s`;
- suspend and hibernate disabled.

## Dead-end actions

| Run mode | `deadend` action | Effect |
|---|---|---|
| In-session | `exit` | Record `deadend`, print the condition and evidence, and exit with a dead-end exit code. |
| Tuning boot | `clear_saved_entry` | Record `deadend`, then run `grub-editenv <grubenv> unset saved_entry` so the next boot selects the first menu entry, the newest normal generation, recorded as `boot.saved_entry` with the entry before and after, or the error. Then record `shutdown` and exit with the dead-end code; the unit does not restart, and the explanation stays on tty1. |
| Tuning boot, boot-loop dead end | `clear_saved_entry_and_reboot` | Same as above, then `systemctl reboot` into the normal system. |

A failed clear never reboots: the next boot would land back in the tuning boot.

## NixOS module

`nixosModules.default` from the flake:

| Option | Meaning |
|---|---|
| `services.shycler.enable` | Install shycler and load `ryzen_smu` (`hardware.cpu.amd.ryzen-smu.enable`, set with `mkDefault`, so a host that loads its own build can turn it off) |
| `services.shycler.tuning.enable` | Add the tuning boot specialisation above |
| `services.shycler.settings` | Freeform attrset rendered to `/etc/shycler/config.toml` |
| `services.shycler.backends.mprime.enable` | Set `settings.backends.mprime` to the nixpkgs `mprime` package (unfree) |
| `services.shycler.backends.ycruncher.enable` | Set `settings.backends.ycruncher` to the nixpkgs `y-cruncher` package (unfree) |

The flake also exposes `packages.x86_64-linux.default` and a dev shell.
