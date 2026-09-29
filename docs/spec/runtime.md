# Runtime

Normative rules for how togi is invoked, configured and deployed.

## Commands

| Command | Writes journal | Purpose |
|---|---|---|
| `togi run [--rotations <N>] [--tuning-boot <grubenv>] [--no-tui]` | yes | Start or resume in the foreground. `--rotations N` (N >= 1) stops after N clean qualifying rotations since the last deepening, only when every core is done and refinement can reach no more depth; without the flag guard runs indefinitely. In a tuning boot the service passes `--tuning-boot`; on a terminal a dashboard replaces event lines unless `--no-tui` is given. |
| `togi status` | no | Replay the journal to show session and BIOS context, activity (`search: N cores left`, `hunt H, mask M`, `refine round R, checks d/t`, `guard rotation N, steps d/t`, or `guard not started`), tier, and missing qualifying coverage; a core table `CORE CCD OFFSET PHASE FAILED JOINT QUEUED LAST DECISION`; joint marks, an open hunt's `MASK CORES OUTCOME STARTS`, an open round's `CHECK CORES PASSES`, and per-workload starts, clean hours and bounds since the tier clock. Also show in-flight action, dead end, carried values, defects and Tctl. |
| `togi cert` | no | Replay the journal for the certificate (`tuner.md`), with SHA-256, a `CORE EDGE FAILED JOINT DONE DECIDED` table and joint marks, evidence since the last failure on a profile at least as deep (or the profile change), and WORKLOAD/STARTS in its evidence table; Silver and Gold use tier-clock hours |
| `togi events` | no | Render the journal with filters (`journal.md`) |
| `togi reset --core <N> \| --all` | yes | Exactly one of the two, else exit 2. `--core N` queues the core's reset and prints `reset of core 03 queued; the next togi run restarts its search from the baseline`. The next `run` clears that core's failed mark and every joint mark that includes it. `--all` archives the session and prints `session <id> archived to archive/<id>.jsonl; the next togi run starts a new session`. After a successful archive it warns on stderr for each configured `candidate_edges.<core>` at or deeper (more negative) than that core's replayed failed mark since its last reset; a joint mark is not a failed mark. The warning does not prevent the reset or carry state into the new session. Missing or invalid configuration skips the check without stopping the archive; it reports the skip only when `--config` was explicitly given. A journal of another schema is archived without replay and skips the check; one of another ruleset is still checked, because failed marks come from its recorded decisions |
| `togi watch [--width <columns>] [--height <rows>]` | no | Replay and redraw the live dashboard each second. It shows search, hunt and refinement activity, done counts, trial progress, guard coverage and tier-clock exposure, joint marks and per-core tiles. The gauge uses `x` for a failed mark, `j` for a joint mark, `>` for current trial offset, `·` for a mask-held anchor and `░` for unexplored depth; labels include `DONE`, `HUNT`, `MASK`. It rereads the journal when `events.jsonl` changes size or modification time and never takes the lock. On a terminal it clears the screen once, then each frame homes the cursor, ends every row with an erase to end of line, erases below the frame, and writes neither the last row nor the last column, so it never scrolls and stray output heals within one frame; it hides the cursor and restores it on SIGINT, SIGTERM or SIGHUP, and redraws on SIGWINCH. Otherwise it prints one frame of `--width` by `--height` (default 240 by 67) without styling and exits. `NO_COLOR` turns colour off. No journal shows "no session yet". Every glyph it draws is in IBM437, the character set of the kernel's built-in console fonts |

`reset` reads the journal before it opens it: no journal or no session exits 1 without creating anything, except that `reset --all` completes an interrupted incompatible archive when the journal has already moved and its pending marker remains, and drops a pending carry. A core that is not in the session exits 2, and a `run` holding the lock exits 3. The reset command records its event in the host's current boot and ends it with `shutdown`, so the next `run` treats that boot as ended cleanly.

A session written by a newer ruleset or schema is refused before another event is appended. `run` checks the journal's build stamp before validating the configuration, so a newer journal still exits 16 when this build also rejects the config file. It reports the writing build's version and advice to install it or run `reset --all`. A session written by an older ruleset or schema, and by no newer one, is not refused by `run`: after reading the configuration, `run` archives it and starts a new session seeded from it (`journal.md`, Transitions; `tuner.md`, Session start), in a tuning boot too. `reset --core` refuses either mismatch, and its message for an older journal names that transition. `status`, `cert` and `events` (including `events --json`) warn to stderr before rendering a different ruleset with this build's rules, but refuse a different schema with a red bold line (journald priority 3). `watch` renders a different ruleset without a warning and shows a different schema's refusal on screen in place of the session. `reset --all` works for either mismatch; a schema mismatch archives without appending to the old journal. `reset --all` also drops a pending carry under the writer lock, exiting 3 while a `run` holds it, so the next session starts over with nothing carried; when the transition has archived the journal and no new one exists yet, it prints `carry from session <id> dropped; the next togi run starts a new session with nothing carried`.

On resume, `run` checks historical decisions for known defects before queued resets and other tuning decisions are consumed. It records `defect.found` once per defect per session, naming affected cores and decision sequences. A too-cautious finding allows guard to continue; `status` shows each affected core's exact reset command until that core is reset. If stdin and stderr are both terminals (verified by a terminal ioctl, not merely character devices), `run` prints the finding and asks `Reset cores 3, 7? [y/N]`; only `y` or `yes`, case-insensitive after trimming a complete input line, queues ordinary `command.reset` events for those cores before tuning resumes, then records `defect.answered`. EOF without a newline, read errors and other answers count as no: they record the answer and continue without resets. Either answer suppresses the prompt on later runs, but does not hide remaining reset guidance from `status`. Without a terminal there is no prompt: a too-cautious defect continues, whereas an unanswered too-aggressive defect stops as a journaled `defect` dead end (exit 17).

Transient backend setup errors and recovery-time kernel-log read errors retry at waits of 60, 300 and 1800 seconds; `backend.retry` records backend (or `kernel_log`), attempt, wait and cause. Interruptions during the wait resume without treating the delay as trial evidence. A missing backend binary is not a transient setup error: it ends as `no_evidence` rather than waiting through retries. A computation error reported just before a crash remains failure evidence even if no `trial.end` was written.

When stdin and stderr are both terminals (the same check) and `--no-tui` is not given, `run` draws the `watch` dashboard on stderr from the moment the session starts until it stops, including the restore after a signal. Event lines are not printed while the dashboard is up; `events.jsonl` still records every event. Output before the session starts, such as a refused configuration, prints as usual. A defect prompt clears the dashboard, asks, then redraws it. When the session stops, `run` clears the screen and prints the outcome: after a clean stop, the closing `profile.restored` line, when the run restored offsets, and the `shutdown` line; otherwise what it prints without the dashboard, the dead end and its evidence, or the error. If the dashboard cannot draw, `run` prints why and falls back to event lines. Without terminals, as under systemd, it prints event lines.

`--config <path>` (default `/etc/togi/config.toml`) and `--state-dir <path>` (default `/var/lib/togi`) are accepted before or after the command. Top-level-only `togi --version` prints `togi x.y.z+rev` to stdout and exits 0 (`go run` and the flake checks use `dev` as the revision); `--version` after a command is an unknown-flag usage error (exit 2). The version is read from root `version.txt` by both Go and the flake; the flake supplies the git revision to `packages.default` at build time.

`togi` without a command prints the wordmark and the tagline `per-core Curve Optimizer` above the usage, to stderr, and exits 2. `--help`, an unknown command and a flag error print the usage alone. The usage holds the synopsis, a description, examples, the commands, shared flags and `--version`.

`togi <command> --help` prints the command's synopsis, a description of what it does, one or two examples, its own flags and then the shared `--config` and `--state-dir` flags (not `--version`), to stdout, and exits 0. A flag error prints the error and the same help to stderr and exits 2. Every flag is shown in its `--long` form.

`run` needs Linux. On any other platform, once the journal compatibility check and the configuration pass, it prints `togi run: hardware runs need Linux: unsupported operation` and exits 1 before it reads the boot id or opens the journal.

Simulation is not part of `togi`: development programs live in `tools/` and are not installed ([ADR 0010](../adr/0010-development-programs-in-tools.md)). [Simulating](../simulating.md) describes `tools/sim`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Stopped cleanly: by signal, or after requested clean qualifying rotations with every core done and no further refinement depth |
| 1 | Error |
| 2 | Usage or configuration error, including a start offset for a core that does not exist |
| 3 | The journal is held by another writer |
| 10 | Dead end `failure_at_zero` |
| 11 | Dead end `smu` |
| 12 | Dead end `no_evidence` |
| 13 | Dead end `boot_loop` |
| 14 | Dead end `containment` |
| 15 | Dead end `preflight` |
| 16 | Dead end: journal schema or ruleset newer than this build's, or, for `reset --core`, different from it |
| 17 | Dead end `defect`: an unanswered too-aggressive defect without a terminal |
| 18 | Dead end `thermal_trip` |

`run` records `shutdown` only for clean stops (exit 0) and journaled dead ends (10-15, 17-18). A compatibility refusal (16) appends nothing. Other exits write nothing, so a later boot treats the gap as a crash, except the journal-write emergency below.

Before it records `shutdown`, a `run` that has written offsets restores them, so the machine goes on running as it did before togi started: each core goes back to its baseline, or to its current offset where that is shallower, clamped to [-50, 0]. A core therefore never stays deeper than togi would run it, and never at or past its failed mark. A signal interrupts the running trial; `run` then records the decisions pending from it and stops before the next trial, so a failure in the interrupted trial is already reflected in the current offset. The writes follow the usual intent, write and readback, then `profile.restored` records the offsets. It skips the writes when the cores already hold those offsets, when this `run` wrote nothing (a `run` that only completes an interrupted dead end included), and at an `smu` dead end, where SMU writes are not trusted. A readback that differs during a clean stop turns it into an `smu` dead end; during another dead end it is recorded as evidence and the dead end stops as planned. In the tuning boot the restore follows the `boot.saved_entry` change. At power-off it is redundant, since firmware restores the BIOS values at the next boot.

If appending an event fails while offsets may be applied, togi stops every backend, writes every core to 0 without an intent, reads each back, reports the original `journal write` error and emergency outcome on stderr/system journal, and stops without further journal events. On the next run the SMU read reconciles hardware with the surviving journal. This narrow emergency is the exception to intent-before-write; failure to restore or read back must also be reported.

## Privileges

`run` and `reset` require root: they change core voltage and the boot entry, and create cgroup scopes. The read-only commands work for any user who can read the state directory.

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

TOML at the `--config` path, produced by the NixOS module from `services.togi.settings`:

| Key | Default | Valid |
|---|---|---|
| `start_offsets.<core>` | none | Offset override per core id, within [-50, 0] |
| `candidate_edges.<core>` | none | Candidate edge in [-50, 0], starting search at that offset with an edge check; not together with `start_offsets` for the same core. A carried mark can clamp it |
| `durations.search_trial_s` | 90 | [1, 86400] |
| `durations.start_s` | 120 | [1, 86400]; short R7 starts, hunt masks, refinement checks and reruns |
| `durations.guard_trial_s` | 120 | [1, 86400] |
| `durations.guard_idle_s` | 900 | [1, 86400]; R6 |
| `durations.guard_all_core_s` | 1200 | [4, 86400]; R7 long starts: a quarter per CCD, remainder all-core |
| `guard.rotation` | `["R7", "R7", "R7", "R2", "R2", "R2", "R6", "R5", "R1", "R1", "R1", "R3", "R4", "R6"]` | Non-empty regime list; clean is not necessarily qualifying |
| `evidence.miss` | 0.05 | Finite number strictly between 0 and 1 |
| `evidence.rate` | 0.5 | Finite number strictly between 0 and 1; together with miss yields at most 1000 starts |
| `dead_ends.inconclusive_in_a_row` | 3 | [1, 100] |
| `dead_ends.stray_crashes_in_a_row` | 3 | [1, 100] |
| `backends.mprime` | not configured | Absolute path of the package (`bin/mprime` inside it) |
| `backends.ycruncher` | not configured | Absolute path of the package (`lib/y-cruncher/Binaries` inside it) |

Unknown keys and out-of-range values are errors. When `--config` is not given and no file exists at the default path, the defaults apply.

`durations.confirmation_trial_s` was removed: a file still setting it fails with `durations.confirmation_trial_s was removed in togi 0.5.0: confirmation no longer exists; delete the key`. Invalid evidence values report `evidence.miss = %g: must be within (0, 1)` (likewise rate); an evidence pair requiring over 1000 starts reports `evidence: miss %g and rate %g need more than 1000 starts per step`.

The number of starts for a full evidence step is `ceil(ln(evidence.miss) / log1p(-evidence.rate))`, five at defaults. Durations and workload identity are part of the trial class, so a change to either never turns passes of a different class into valid evidence.

The effective configuration is recorded in `config.loaded` at every start. Configuration that changes the meaning of existing evidence, such as trial durations, is allowed mid-session and takes effect from the next trial. The journal shows when it changed.

The TOML configuration has no version field. Renamed or removed options are handled in the NixOS module with `mkRenamedOptionModule` or `mkRemovedOptionModule` and noted in the changelog; changed configurable defaults are not a ruleset break.

## In-session run

`sudo togi run` on a normal boot. If the machine crashes, it comes back to the normal desktop. The next `sudo togi run` detects the crash from the journal, attributes it to the in-flight action, and continues. Nothing restarts togi automatically outside a tuning boot.

## Tuning boot

`services.togi.tuning.enable` adds a GRUB specialisation named `togi` and makes GRUB remember the last booted entry (`boot.loader.grub.default = "saved"`). Picking "togi" once in the menu starts unattended tuning. Every crash reboot returns to it until a dead end, an orderly shutdown or reboot (unless `tuning.leaveOnShutdown` is off), or you, select a normal entry. The module asserts that GRUB is the bootloader; other bootloaders are [#18](https://github.com/shgew/togi/issues/18).

Inside the specialisation:
- `systemd.defaultUnit = "multi-user.target"`, so no graphical session starts;
- `togi.service` runs `togi run --tuning-boot <grubenv>` as root, with `Restart=on-failure`, `RestartSec=60`, a start limit of 3 per 30 minutes, and `RestartPreventExitStatus` listing dead-end exit codes 10-18;
- `togi.service` has `OnFailure=togi-restart-limit.service`. On restart-limit failure this oneshot checks `MONITOR_EXIT_STATUS`, ignores dead-end codes 10 through 18, otherwise runs `grub-editenv <grubenv> unset saved_entry` and then `systemctl reboot` into the normal system. A failed clear does not reboot; dead ends retain their own boot action and dashboard;
- with `tuning.leaveOnShutdown` (the default), `togi-leave-tuning-boot.service`, a oneshot unit ordered before `togi.service`, so it stops after it, whose `ExecStop` runs `grub-editenv <grubenv> unset saved_entry`. An orderly shutdown or reboot therefore hands the next boot to the newest normal generation; a panic, a watchdog reset or a power loss never runs it, so crash reboots stay in the tuning boot. The clear is logged to the system journal, not to togi's journal. The unit is not restarted when a configuration switch changes it;
- `togi-watch.service` runs `togi watch` on tty1 with `TERM=linux`, `Restart=always` and `Nice=-10`, in place of the tty1 login prompt. `togi.service` keeps the default niceness: its backends run as children of its scopes and would inherit it;
- `togi-console.service` follows `togi.service`'s log on tty3, in place of the tty3 login prompt; tty2 keeps its login;
- nothing else writes to tty1. The kernel command line is the system's, without its `console=ttyN` entries, plus `console=tty3`; it replaces kernel parameters defined for the specialisation alone, since a module cannot remove entries from a list option otherwise, so `/dev/console` output (early boot, systemd status, the emergency shell, panic text) goes to tty3; serial consoles stay. `console=` only picks the terminal behind `/dev/console`: the kernel prints its messages on the foreground terminal unless redirected, so `togi-kernel-log.service`, a oneshot ordered before the dashboard, runs `setlogcons 3` to send them to tty3 as well. Alt+F3 shows the boot log and kernel messages;
- `console.font` forced to `tuning.consoleFont`, by default `null`, the kernel's built-in font, so a system font without IBM437's block glyphs never reaches the dashboard;
- sysctls `kernel.panic=10`, `kernel.panic_on_oops=1`, `kernel.hardlockup_panic=1`, `kernel.softlockup_panic=1`;
- `systemd.settings.Manager.RuntimeWatchdogSec = "30s"`, so the SP5100 TCO hardware watchdog resets a frozen machine;
- journald `Storage=persistent` and `SyncIntervalSec=1s`;
- suspend and hibernate disabled.

## Dead-end actions

| Run mode | `deadend` action | Effect |
|---|---|---|
| In-session | `exit` | Record `deadend`, print the condition and evidence, and exit with a dead-end exit code. |
| Tuning boot | `clear_saved_entry` | Record `deadend`, then run `grub-editenv <grubenv> unset saved_entry` so the next boot selects the first menu entry, the newest normal generation, recorded as `boot.saved_entry` with the entry before and after, or the error. Then record `shutdown` and exit with the dead-end code; the unit does not restart, and the dashboard on tty1 shows the dead end. |
| Tuning boot, journal with a newer ruleset or schema | `clear_saved_entry` | Print the refusal to stderr, clear GRUB's saved entry without journaling in the incompatible journal, report whether the clear succeeded, and exit 16 without rebooting. |
| Tuning boot, boot-loop dead end | `clear_saved_entry_and_reboot` | Same as above, then `systemctl reboot` into the normal system. |

A failed clear never reboots: the next boot would land back in the tuning boot.

If a `run` starts after `deadend` but before `boot.saved_entry`, it completes the recorded action before any preflight, SMU write or trial: it clears GRUB's saved entry, appends `boot.saved_entry`, records `shutdown`, and exits with the same dead-end condition and evidence. A `clear_saved_entry_and_reboot` action still requests a reboot after a successful clear; a failed clear records the error and exits without rebooting. A `run` without `--tuning-boot` cannot clear the entry: it records `shutdown` and exits with the dead-end condition, leaving GRUB's saved entry as the operator last chose it. If `boot.saved_entry` exists but `shutdown` does not, the next `run` records `shutdown` and exits without clearing again or tuning. An interrupted in-session `exit` action likewise records `shutdown` and exits. Once the dead end and shutdown are fully recorded, a later `run` starts normally and re-evaluates the conditions; a still-failing preflight stops before tuning.

## NixOS module

`nixosModules.default` from the flake:

| Option | Meaning |
|---|---|
| `services.togi.enable` | Install togi and load `ryzen_smu` (`hardware.cpu.amd.ryzen-smu.enable`, set with `mkDefault`, so a host that loads its own build can turn it off) |
| `services.togi.tuning.enable` | Add the tuning boot specialisation above |
| `services.togi.tuning.leaveOnShutdown` | Default `true`: an orderly shutdown or reboot of the tuning boot clears GRUB's saved entry, so the next boot is the normal system. `false` keeps the tuning boot selected until a dead end or you pick another entry |
| `services.togi.tuning.consoleFont` | The tuning boot's console font, as `console.font` takes it, whatever the system sets. Default `null`: the kernel's built-in font, 8x16 below 2560x1080 and Terminus 16x32 bold from there. Both give the 240x67 frame the dashboard is laid out for at 1080p and 4K, and both cover IBM437, whose block and box glyphs the dashboard draws with; a system font such as `Lat2-Terminus16` lacks `▀` and breaks the big digits. A font set here must cover IBM437 as well: Terminus' `ter-i` fonts, such as `"${pkgs.terminus_font}/share/consolefonts/ter-i32b.psf.gz"`, do, while `ter-v` and `Lat2-Terminus` fonts do not |
| `services.togi.settings` | Freeform attrset rendered to `/etc/togi/config.toml` |
| `services.togi.backends.mprime.enable` | Set `settings.backends.mprime` to the nixpkgs `mprime` package (unfree) |
| `services.togi.backends.ycruncher.enable` | Set `settings.backends.ycruncher` to the nixpkgs `y-cruncher` package (unfree) |

The flake also exposes `packages.x86_64-linux.default` and a dev shell, and the same for `aarch64-darwin` for development, where the checks are `package`, `lint` and `fmt`, without the VM test. The package builds only `cmd/togi` and runs the tests of every package, so the installed package contains only the `togi` executable. The release procedure and version bump rules are in [Releasing](../releasing.md).
