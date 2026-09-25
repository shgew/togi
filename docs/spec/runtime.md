# Runtime

Normative rules for how shycler is invoked, configured and deployed.

## Commands

| Command | Writes journal | Purpose |
|---|---|---|
| `shycler run [--sim <seed>] [--rotations <N>] [--tuning-boot <grubenv>]` | yes | Start or resume the session in the foreground. The same entry point serves in-session runs and the tuning boot service, which passes `--tuning-boot` with its GRUB environment file (Dead-end actions). `--sim` and `--tuning-boot` together are a usage error, so a simulated dead end never touches the host's boot entry. With `--rotations N` (N >= 1) it stops, recording `shutdown`, when a rotation would start after the current profile survived N clean guard rotations; without it guard is endless |
| `shycler status` | no | Session and BIOS context; phase, tier and guard progress; in-flight action and dead end; per-core table of offset, phase, failed mark, unproven depth, queued command and last decision; too-cautious defects with one `shycler reset --core N` command per affected core not yet reset since the finding, even if the prompt was declined; clean hours and failure-rate bound overall and per regime, and the highest Tctl. Rendered from a replay of the journal, not `state.json` |
| `shycler cert` | no | Render the certificate (`tuner.md`) from a replay of the journal, with the SHA-256 of the lines it rendered |
| `shycler events` | no | Render the journal with filters (`journal.md`) |
| `shycler regain [--core <N>]` | yes | Queue one count of regain on every confirmed core with unproven depth, or on core N only (`tuner.md`). Prints `regain queued for cores 03, 07; the next shycler run confirms one count deeper on each`. Exit 1 when a regain is already queued or running, or nothing is left to regain |
| `shycler reset --core <N> \| --all` | yes | Exactly one of the two, else exit 2. `--core N` queues the core's reset and prints `reset of core 03 queued; the next shycler run restarts its search from the baseline`. `--all` archives the session and prints `session <id> archived to archive/<id>.jsonl; the next shycler run starts a new session` |

`regain` and `reset` read the journal before they open it: no journal or no session exits 1 without creating anything, except that `reset --all` completes an interrupted incompatible archive when the journal has already moved and its pending marker remains. A core that is not in the session exits 2, and a `run` holding the lock exits 3. Both record their events in the host's current boot, and end it with `shutdown`, so the next `run` treats that boot as ended cleanly.
An incompatible session is refused before another event is appended. `run` checks the journal's build stamp before validating the configuration, so a mismatched journal still exits 16 when this build also rejects the config file. It reports the writing build's version and advice to install it or run `reset --all`. `regain` and `reset --core` also refuse either mismatch. `status`, `cert` and `events` (including `events --json`) warn to stderr before rendering a different ruleset with this build's rules, but refuse a different schema with a red bold line (journald priority 3). `reset --all` works for either mismatch; a schema mismatch archives without appending to the old journal.

On resume, `run` checks historical decisions for known defects before queued resets and other tuning decisions are consumed. It records `defect.found` once per defect per session, naming affected cores and decision sequences. A too-cautious finding allows guard to continue; `status` shows each affected core's exact reset command until that core is reset. If stdin and stderr are both terminals (verified by a terminal ioctl, not merely character devices), `run` prints the finding and asks `Reset cores 3, 7? [y/N]`; only `y` or `yes`, case-insensitive after trimming a complete input line, queues ordinary `command.reset` events for those cores before tuning resumes, then records `defect.answered`. EOF without a newline, read errors and other answers count as no: they record the answer and continue without resets. Either answer suppresses the prompt on later runs, but does not hide remaining reset guidance from `status`. Without a terminal there is no prompt: a too-cautious defect continues, whereas an unanswered too-aggressive defect stops as a journaled `defect` dead end (exit 17).

`--config <path>` (default `/etc/shycler/config.toml`) and `--state-dir <path>` (default `/var/lib/shycler`) are accepted before or after the command. Top-level-only `shycler --version` prints `shycler x.y.z+rev` to stdout and exits 0 (`go run` uses `dev` as the revision); `--version` after a command is an unknown-flag usage error (exit 2). The version is read from root `version.txt` by both Go and the flake; the flake supplies the git revision at build time.

`shycler` without a command prints the wordmark and the tagline `per-core Curve Optimizer` above the usage, to stderr, and exits 2. `--help`, an unknown command and a flag error print the usage alone. The usage holds the synopsis, a description, examples, the commands, shared flags and `--version`.

`shycler <command> --help` prints the command's synopsis, a description of what it does, one or two examples, its own flags and then the shared `--config` and `--state-dir` flags (not `--version`), to stdout, and exits 0. A flag error prints the error and the same help to stderr and exits 2. Every flag is shown in its `--long` form.

`shycler run --sim <seed>` drives the seeded simulator (`internal/sim`) instead of hardware: a 16-core machine whose crash reboots are handled in-process, with a real journal. It uses `--state-dir` when given, else a new temporary directory whose path it prints to stderr, and it fsyncs nothing. With `--sim`, `--rotations` defaults to 1. A state directory that already holds a journal or archives resumes the simulated machine after them: boot numbering continues and the clock starts after the last event, so a crash in the new run is never mistaken for an old boot, and a session after `reset --all` gets a new id.

`run` without `--sim` needs Linux. On any other platform it prints `shycler run: hardware runs need Linux: unsupported operation` and exits 1 before it reads the boot id or opens the journal.

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
| 16 | Dead end: journal schema or ruleset incompatible with this build |
| 17 | Dead end `defect`: an unanswered too-aggressive defect without a terminal |

`run` records `shutdown` only for clean stops (exit 0) and journaled dead ends (10-15, 17). A compatibility refusal (16) appends nothing, even when clearing GRUB's saved entry. Any other exit writes nothing, so the next boot treats the gap as a crash.

Before it records `shutdown`, a `run` that has written offsets restores them, so the machine goes on running as it did before shycler started: each core goes back to its baseline, or to its current offset where that is shallower, clamped to [-50, 0]. A core therefore never stays deeper than shycler would run it, and never at or past its failed mark. A signal interrupts the running trial; `run` then records the decisions pending from it and stops before the next trial, so a failure in the interrupted trial is already reflected in the current offset. The writes follow the usual intent, write and readback, then `profile.restored` records the offsets. It skips the writes when the cores already hold those offsets, when this `run` wrote nothing (a `run` that only completes an interrupted dead end included), and at an `smu` dead end, where SMU writes are not trusted. A readback that differs during a clean stop turns it into an `smu` dead end; during another dead end it is recorded as evidence and the dead end stops as planned. In the tuning boot the restore follows the `boot.saved_entry` change. At power-off it is redundant, since firmware restores the BIOS values at the next boot.

## Privileges

`run`, `regain` and `reset` require root: they change core voltage and the boot entry, and create cgroup scopes. The read-only commands work for any user who can read the state directory.

## Preflight

`run` checks, each recorded as a `preflight.check` event:
1. Running as root.
2. The CPU is family `0x1A`, model `0x40`-`0x4F` (Granite Ridge desktop).
3. `ryzen_smu` is loaded and reports a matching codename.
4. Every core's offset reads back through the SMU.
5. The core-to-SMU slot mapping is verified (`../prior-art.md`): each CCD fuse is read twice with distinct RSMU register reads interleaved, and the disabled-slot count must match the OS topology. Indistinguishable RSMU probes, inconsistent fuse reads, or a live-core count mismatch refuse all per-core access.
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

The TOML configuration has no version field. Renamed or removed options are handled in the NixOS module with `mkRenamedOptionModule` or `mkRemovedOptionModule` and noted in the changelog; changed configurable defaults are not a ruleset break.

## In-session run

`sudo shycler run` on a normal boot. If the machine crashes, it comes back to the normal desktop. The next `sudo shycler run` detects the crash from the journal, attributes it to the in-flight action, and continues. Nothing restarts shycler automatically outside a tuning boot.

## Tuning boot

`services.shycler.tuning.enable` adds a GRUB specialisation named `shycler` and makes GRUB remember the last booted entry (`boot.loader.grub.default = "saved"`). Picking "shycler" once in the menu starts unattended tuning. Every crash reboot returns to it until a dead end, or you, select a normal entry. The module asserts that GRUB is the bootloader; other bootloaders are [#27](https://code.marleb.org/shgew/shycler/issues/27).

Inside the specialisation:
- `systemd.defaultUnit = "multi-user.target"`, so no graphical session starts;
- `shycler.service` runs `shycler run --tuning-boot <grubenv>` as root, where `<grubenv>` is the GRUB environment under the first mirrored boot directory, with `Restart=on-failure`, `RestartSec=60`, a start limit of 3 per 30 minutes, and `RestartPreventExitStatus` listing dead-end exit codes 10-17;
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
| Tuning boot, incompatible journal | `clear_saved_entry` | Print the refusal to stderr, clear GRUB's saved entry without journaling in the incompatible journal, report whether the clear succeeded, and exit 16 without rebooting. |
| Tuning boot, boot-loop dead end | `clear_saved_entry_and_reboot` | Same as above, then `systemctl reboot` into the normal system. |

A failed clear never reboots: the next boot would land back in the tuning boot.

If a `run` starts after `deadend` but before `boot.saved_entry`, it completes the recorded action before any preflight, SMU write or trial: it clears GRUB's saved entry, appends `boot.saved_entry`, records `shutdown`, and exits with the same dead-end condition and evidence. A `clear_saved_entry_and_reboot` action still requests a reboot after a successful clear; a failed clear records the error and exits without rebooting. A `run` without `--tuning-boot` cannot clear the entry: it records `shutdown` and exits with the dead-end condition, leaving GRUB's saved entry as the operator last chose it. If `boot.saved_entry` exists but `shutdown` does not, the next `run` records `shutdown` and exits without clearing again or tuning. An interrupted in-session `exit` action likewise records `shutdown` and exits. Once the dead end and shutdown are fully recorded, a later `run` starts normally and re-evaluates the conditions; a still-failing preflight stops before tuning.

## NixOS module

`nixosModules.default` from the flake:

| Option | Meaning |
|---|---|
| `services.shycler.enable` | Install shycler and load `ryzen_smu` (`hardware.cpu.amd.ryzen-smu.enable`, set with `mkDefault`, so a host that loads its own build can turn it off) |
| `services.shycler.tuning.enable` | Add the tuning boot specialisation above |
| `services.shycler.settings` | Freeform attrset rendered to `/etc/shycler/config.toml` |
| `services.shycler.backends.mprime.enable` | Set `settings.backends.mprime` to the nixpkgs `mprime` package (unfree) |
| `services.shycler.backends.ycruncher.enable` | Set `settings.backends.ycruncher` to the nixpkgs `y-cruncher` package (unfree) |

The flake also exposes `packages.x86_64-linux.default` and a dev shell, and the same for `aarch64-darwin` for development, where the checks are `package` and `lint` without the VM test. The installed package contains only the `shycler` executable. The release procedure and version bump rules are in [Releasing](../releasing.md).
