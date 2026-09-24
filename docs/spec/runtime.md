# Runtime

Normative rules for how shycler is invoked, configured and deployed.

## Commands

| Command | Writes journal | Purpose |
|---|---|---|
| `shycler run [--sim <seed>] [--rotations N]` | yes | Start or resume the session in the foreground. The same entry point serves in-session runs and the tuning boot service. With `--rotations N` (N >= 1) it stops, recording `shutdown`, when a rotation would start after the current profile survived N clean guard rotations; without it guard is endless |
| `shycler status` | no | Per-core table: offset, phase, failed mark, unproven depth, clean hours per regime; plus tier and in-flight action |
| `shycler cert` | no | Render the certificate (`tuner.md`) |
| `shycler events` | no | Render the journal with filters (`journal.md`) |
| `shycler regain [--core N]` | yes | Queue regain of unproven depth; refuses while `run` holds the lock |
| `shycler reset --core N \| --all` | yes | Reset one core or archive the session; refuses while `run` holds the lock |

Global flags: `--config <path>` (default `/etc/shycler/config.toml`), `--state-dir <path>` (default `/var/lib/shycler`).

`shycler run --sim <seed>` drives the seeded simulator (`internal/sim`) instead of hardware: a 16-core machine whose crash reboots are handled in-process, with a real journal. It uses `--state-dir` when given, else a new temporary directory whose path it prints to stderr, and it fsyncs nothing. With `--sim`, `--rotations` defaults to 1. A state directory that already holds a journal resumes the simulated machine after it: boot numbering continues and the clock starts after the last event, so a crash in the new run is never mistaken for an old boot. Until the hardware seams exist (T09-T12), `run` without `--sim` refuses with exit code 2.

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
6. The configured backends are present.
7. `systemd-run` is available.
8. The BIOS context matches the session, when resuming.

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
| `backends.mprime` | not configured | Absolute path |
| `backends.ycruncher` | not configured | Absolute path |

Unknown keys and out-of-range values are errors. When `--config` is not given and no file exists at the default path, the defaults apply.

The effective configuration is recorded in `config.loaded` at every start. Configuration that changes the meaning of existing evidence, such as trial durations, is allowed mid-session and takes effect from the next trial. The journal shows when it changed.

## In-session run

`sudo shycler run` on a normal boot. If the machine crashes, it comes back to the normal desktop. The next `sudo shycler run` detects the crash from the journal, attributes it to the in-flight action, and continues. Nothing restarts shycler automatically outside a tuning boot.

## Tuning boot

`services.shycler.tuning.enable` adds a GRUB specialisation named `shycler` and makes GRUB remember the last booted entry (`boot.loader.grub.default = "saved"`). Picking "shycler" once in the menu starts unattended tuning. Every crash reboot returns to it until a dead end, or you, select a normal entry. The module asserts that GRUB is the bootloader; other bootloaders are on the roadmap.

Inside the specialisation:
- `systemd.defaultUnit = "multi-user.target"`, so no graphical session starts;
- `shycler.service` runs `shycler run` as root with `Restart=on-failure` and `RestartSec=60`, and a start limit of 3 per 30 minutes. `RestartPreventExitStatus` lists the dead-end exit codes 10-15;
- a tty1 unit follows `shycler.service`'s log;
- sysctls `kernel.panic=10`, `kernel.panic_on_oops=1`, `kernel.hardlockup_panic=1`, `kernel.softlockup_panic=1`;
- `systemd.settings.Manager.RuntimeWatchdogSec = "30s"`, so the SP5100 TCO hardware watchdog resets a frozen machine;
- journald `Storage=persistent` and `SyncIntervalSec=1s`;
- suspend and hibernate disabled.

## Dead-end actions

| Run mode | Action |
|---|---|
| In-session | Record `deadend`, print the condition and evidence, and exit with a dead-end exit code. |
| Tuning boot | Record `deadend`, then clear GRUB's saved entry so the next boot selects the newest normal generation, recorded as `boot.saved_entry`. shycler then stays stopped at the console with the explanation on tty1. |
| Tuning boot, boot-loop dead end | Same as above, then reboot immediately into the normal system. |

The exact `grub-editenv` invocation and the fallback entry are verified in the NixOS module task.

## NixOS module

`nixosModules.default` from the flake:

| Option | Meaning |
|---|---|
| `services.shycler.enable` | Install shycler and load `ryzen_smu` |
| `services.shycler.tuning.enable` | Add the tuning boot specialisation above |
| `services.shycler.settings` | Freeform attrset rendered to `/etc/shycler/config.toml` |
| `services.shycler.backends.mprime.enable` | Provide mprime (unfree) |
| `services.shycler.backends.ycruncher.enable` | Provide y-cruncher (unfree) |

The flake also exposes `packages.x86_64-linux.default` and a dev shell.
