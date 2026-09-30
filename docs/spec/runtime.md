# Runtime

Normative rules for how togi is invoked, configured and deployed.

## Commands

| Command | Writes journal | Purpose |
|---|---|---|
| `togi run [--rotations <N>] [--tuning-boot <grubenv>] [--no-tui]` | yes | Start or resume in the foreground. `--rotations N` (N >= 1) stops after N clean qualifying rotations since the last deepening, only when every core is done and refinement can reach no more depth; without the flag guard runs indefinitely. In a tuning boot the service passes `--tuning-boot`, which requires an armed hardware watchdog before tuning; on a terminal a dashboard replaces event lines unless `--no-tui` is given. |
| `togi status` | no | Replay the journal to show session and BIOS context, activity (`search: N cores left`, `hunt H, mask M`, `refine round R, checks d/t`, `guard rotation N, steps d/t`, or `guard not started`), tier, and missing qualifying coverage; a core table `CORE CCD OFFSET PHASE FAILED JOINT QUEUED LAST DECISION`; joint marks, an open hunt's `MASK CORES OUTCOME STARTS`, an open round's `CHECK CORES PASSES`, and per-workload STARTS (passes valid for the current profile since each trial class's latest relevant failure), CLEAN H and bounds (since the tier clock). Also show in-flight action, dead end, carried values, defects and Tctl. |
| `togi cert` | no | Replay the journal for the certificate (`tuner.md`), with SHA-256, a `CORE CCD SLOT OFFSET FAILED JOINT DONE DECIDED` table and joint marks, evidence since the last failure on a profile at least as deep (or the profile change), and WORKLOAD/STARTS in its evidence table; Silver and Gold use tier-clock hours |
| `togi events` | no | Render the journal with filters (`journal.md`) |
| `togi reset --core <N> \| --all` | yes | Exactly one of the two, else exit 2. `--core N` queues the core's reset and prints `reset of core 03 queued; the next togi run restarts its search from the baseline`. The next `run` clears that core's failed mark and every joint mark that includes it. `--all` archives the session and prints `session <id> archived to archive/<id>.jsonl; the next togi run starts a new session`. After a successful archive it warns on stderr for each configured `candidate_edges.<core>` at or deeper (more negative) than that core's replayed failed mark since its last reset; a joint mark is not a failed mark. The warning does not prevent the reset or carry state into the new session. Missing or invalid configuration skips the check without stopping the archive; it reports the skip only when `--config` was explicitly given. A journal of another schema is archived without replay and skips the check; one of another ruleset is still checked, because failed marks come from its recorded decisions |
| `togi watch [--width <columns>] [--height <rows>]` | no | Replay and redraw the live dashboard each second. It shows search, hunt and refinement activity, done counts, trial progress, guard coverage and tier-clock exposure, joint marks and per-core tiles. The gauge uses `x` for a failed mark, `j` for a joint mark, `>` for current trial offset, `·` for a mask-held anchor and `░` for unexplored depth; labels include `DONE`, `HUNT`, `MASK`. It rereads the journal when `events.jsonl` changes size or modification time and never takes the lock. On a terminal it clears the screen once, then each frame homes the cursor, ends every row with an erase to end of line, erases below the frame, and writes neither the last row nor the last column, so it never scrolls and stray output heals within one frame; it hides the cursor and restores it on SIGINT, SIGTERM or SIGHUP, and redraws on SIGWINCH. Otherwise it prints one frame of `--width` by `--height` (default 240 by 67) without styling and exits. `NO_COLOR` turns colour off. No journal shows "no session yet". Every glyph it draws is in IBM437, the character set of the kernel's built-in console fonts |

The certificate's `OFFSET` is the resident profile offset when guard exists, otherwise the core's current offset, not its checked isolated edge. `CCD` comes from the recorded core topology and `SLOT` is the core number modulo 8; per-core access requires full CCDs (`Preflight`). Match each row to the BIOS per-core Curve Optimizer controls by CCD and slot. Board labels vary: confirm every row before saving and stop if the labels cannot be reconciled.

`reset` reads the journal before it opens it: no journal or no session exits 1 without creating state files, except that `reset --all` completes an interrupted incompatible archive when the journal has already moved and its pending marker remains, and drops a pending carry. A core that is not in the session exits 2, and contention on the host or state-directory lock exits 3. The reset command records its event in the host's current boot and ends it with `shutdown`, so the next `run` treats that boot as ended cleanly.

A session written by a newer ruleset or schema is refused before another event is appended. `run` checks the journal's build stamp before validating the configuration, so a newer journal still exits 16 when this build also rejects the config file. It reports the writing build's version and advice to install it or run `reset --all`. A session written by an older ruleset or schema, and by no newer one, is not refused by `run`: after reading the configuration, `run` archives it and starts a new session seeded from it (`journal.md`, Transitions; `tuner.md`, Session start), in a tuning boot too. `reset --core` refuses either mismatch, and its message for an older journal names that transition. `status`, `cert` and `events` (including `events --json`) warn to stderr before rendering a different ruleset with this build's rules, but refuse a different schema with a red bold line (journald priority 3). `watch` renders a different ruleset without a warning and shows a different schema's refusal on screen in place of the session. `reset --all` works for either mismatch; a schema mismatch archives without appending to the old journal. `reset --all` also drops a pending carry under the writer lock, exiting 3 while a `run` holds it, so the next session starts over with nothing carried; when the transition has archived the journal and no new one exists yet, it prints `carry from session <id> dropped; the next togi run starts a new session with nothing carried`.

On resume, `run` checks historical decisions for known defects before queued resets and other tuning decisions are consumed. It records `defect.found` once per defect per session, naming affected cores and decision sequences. A too-cautious finding allows guard to continue; `status` shows each affected core's exact reset command until that core is reset. If stdin and stderr are both terminals (verified by a terminal ioctl, not merely character devices), `run` prints the finding and asks `Reset cores 3, 7? [y/N]`; only `y` or `yes`, case-insensitive after trimming a complete input line, queues ordinary `command.reset` events for those cores before tuning resumes, then records `defect.answered`. EOF without a newline, read errors and other answers count as no: they record the answer and continue without resets. Either answer suppresses the prompt on later runs, but does not hide remaining reset guidance from `status`. Without a terminal there is no prompt: a too-cautious defect continues, whereas an unanswered too-aggressive defect stops as a journaled `defect` dead end (exit 17).

Recovery-time kernel-log read errors retry at waits of 60, 300 and 1800 seconds. A transient backend setup error makes its trial inconclusive and counts toward the backend's streak: after a backend has `dead_ends.inconclusive_in_a_row` consecutive inconclusive trials, setup errors included, the next three trials for that backend wait 60, 300 and 1800 seconds respectively. `backend.retry` records backend (or `kernel_log`), attempt, wait and cause. If all three are inconclusive too, `no_evidence` fires at `dead_ends.inconclusive_in_a_row + 3` consecutive inconclusive trials. Interruptions during the wait resume without treating the delay as trial evidence; a stop signal during either wait records `shutdown` and exits 0. A missing backend binary is not a transient setup error: it ends as `no_evidence` rather than waiting through retries. A computation error reported just before a crash remains failure evidence even if no `trial.end` was written.

When stdin and stderr are both terminals (the same check) and `--no-tui` is not given, `run` draws the `watch` dashboard on stderr from the moment the session starts until it stops, including the restore after a signal. Event lines are not printed while the dashboard is up; `events.jsonl` still records every event. Output before the session starts, such as a refused configuration, prints as usual. A defect prompt clears the dashboard, asks, then redraws it. When the session stops, `run` clears the screen and prints the outcome: after a clean stop, the closing `profile.restored` line, when the run restored offsets, and the `shutdown` line; otherwise what it prints without the dashboard, the dead end and its evidence, or the error. If the dashboard cannot draw, `run` prints why and falls back to event lines. Without terminals, as under systemd, it prints event lines.

The clean-stop summary ignores interleaved or trailing `session.warning` events when selecting the closing restoration and shutdown lines, preserving their journal order. It stops at any other event kind or a preceding shutdown, so earlier restoration and shutdown lines are not repeated as the current run's outcome.

`--state-dir <path>` (default `/var/lib/togi`) is shared by every command and accepted before or after the command name. `--config <path>` (default `/etc/togi/config.toml`) is accepted before or after the command name only for `run` and `reset`. `status`, `cert`, `events` and `watch` reject an explicit `--config` with a usage error (exit 2).

Top-level-only `togi --version` prints `togi x.y.z+rev` to stdout and exits 0; `--version` after a command is an unknown-flag usage error (exit 2). The version is read from root `version.txt` by both Go and the flake. An explicit revision supplied with `-X github.com/shgew/togi.rev=...` takes precedence, as for the flake's `packages.default`. Otherwise the revision comes from Go's VCS build information: the first seven characters of `vcs.revision`, followed by `-dirty` when `vcs.modified` is true. Without either revision it is `dev`; `go run` and the flake checks continue to report `dev`.

`togi` without a command prints the wordmark and the tagline `per-core Curve Optimizer` above the usage, to stderr, and exits 2. `--help`, an unknown command and a flag error print the usage alone. The usage holds the synopsis, a description, examples, the commands, shared flags and `--version`.

`togi <command> --help` prints the command's synopsis, a description of what it does, one or two examples, its own flags and then the shared `--state-dir` flag (not `--version`), to stdout, and exits 0. Only `run` and `reset` list `--config` among their own flags. A flag error prints the error and the same help to stderr and exits 2. Every flag is shown in its `--long` form.

`run` needs Linux. On any other platform, once the journal compatibility check and the configuration pass, it prints `togi run: hardware runs need Linux: unsupported operation` and exits 1 before it reads the boot id or opens the journal.

Simulation is not part of `togi`: development programs live in `tools/` and are not installed ([ADR 0010](../adr/0010-development-programs-in-tools.md)). [Simulating](../simulating.md) describes `tools/sim`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Stopped cleanly: by signal, or after requested clean qualifying rotations with every core done and no further refinement depth |
| 1 | Error |
| 2 | Usage or configuration error, including a start offset for a core that does not exist |
| 3 | The host lock or journal is held by another writer |
| 10 | Dead end `failure_at_zero` |
| 11 | Dead end `smu` |
| 12 | Dead end `no_evidence` |
| 13 | Dead end `boot_loop` |
| 14 | Dead end `containment` |
| 15 | Dead end `preflight` |
| 16 | Dead end: journal schema or ruleset newer than this build's, or, for `reset --core`, different from it |
| 17 | Dead end `defect`: an unanswered too-aggressive defect without a terminal |
| 18 | Dead end `thermal_trip` |

Read-only commands (`status`, `cert`, `events` and one-frame `watch`) exit 2 for usage errors and 1 for an unreadable or incompatible journal, with the error on stderr. A valid query with no results exits 0 with empty stdout. `watch` is the dashboard exception: without a terminal it still prints a problem frame on stdout before exiting 1, and a missing journal prints `no session yet` and exits 0 with empty stderr. Live `watch` keeps displaying journal problems in the dashboard without exiting. A readable journal with a different ruleset remains a warning for `status`, `cert` and `events`, as described above.

One-frame `watch` strips ANSI terminal sequences from both its frame and its journal-error diagnostic.

`run` records `shutdown` only for clean stops (exit 0) and journaled dead ends (10-15, 17-18), and only after required same-boot reconciliation succeeds. A compatibility refusal (16) appends nothing. Other exits may record restoration but no `shutdown`, so a later boot treats the gap as a crash, except the journal-write emergency below.

Once the host lock is held and hardware is set up, one deferred run owner handles every exit: ordinary error, signal, requested rotations, dead end and panic. It cancels the running workload and joins its teardown before restoring offsets, then closes the journal before releasing the host lock. If teardown cannot confirm that the workload stopped, ordinary restoration and `shutdown` are withheld; the cleanup error is reported and same-boot reconciliation remains necessary. Journal-write failures retain the emergency restoration below. Cleanup errors are joined to the original error, not substituted for it. A panic runs the same cleanup and then propagates the original panic. SIGKILL, a kernel crash and power loss cannot run deferred cleanup; firmware resets offsets on reboot.

On a stop signal, after workload teardown joins and before restoration, the owner drains failure attribution, marks and backoffs required by evidence already durable in the journal. The drain can close a completed hunt or a failed refinement round, but starts no trial, hunt, mask, deepening or refinement move, reads no hardware ranking and waits for no new evidence. Its bound is the finite outstanding commitments of those recorded failures and completed masks; unresolved hunts remain for the next run. A failure at 0 retains its normal dead-end outcome.

A run that has written offsets, or read back offsets inherited from a same-boot run, restores each core to its baseline, or to its current offset where that is shallower, clamped to [-50, 0] and shallower than its recorded failed mark. The restored profile reaches no joint mark. The writes follow the usual intent, write and readback, then `profile.restored` records the offsets. It skips writes when the cores already hold those offsets, when this run wrote nothing and inherited no offsets, and during cleanup at an `smu` dead end where writes are not trusted. A differing readback during a clean stop turns it into an `smu` dead end; another dead end retains its original outcome. In the tuning boot ordinary exit restoration follows the `boot.saved_entry` change; startup reconciliation precedes a pending dead end's completion.

A durable passing `trial.end` is authoritative. Failure to write the trial directory's `passed` marker or prune older passing directories appends `session.warning`, caused by that trial end, and does not stop tuning. A missing marker only retains that directory on disk. A failure to append the warning still follows the journal-write emergency.

If appending an event fails after CPU family/model and driver codename validation has established that SMU commands are safe, togi stops every backend, writes every core to 0 without an intent, reads each back, reports the original `journal write` error and emergency outcome on stderr/system journal, and stops without further journal events. Before that validation boundary, a journal failure remains fatal but performs no mailbox command or SMN access: this process cannot have written an offset. On the next run the SMU read reconciles hardware with the surviving journal. This narrow emergency is the exception to intent-before-write; failure to restore or read back must also be reported. Projection-write failures remain warnings; failure to append their warning follows the same validation boundary.

## Host lock

`run`, `reset` and every `hardware`-tagged test take an exclusive, non-blocking `flock` on `/run/lock/togi.lock` on Linux. On Darwin, `reset` of a copied journal uses `/tmp/togi.lock`; hardware tuning remains Linux-only. The lock is shared across all state directories. `run` takes it before constructing hardware, reading the BIOS context or preparing a carry, and holds it until workloads are stopped, offsets restored and the journal closed. `reset` takes it before changing the session and holds it until its journal is closed. Hardware tests hold it through workload teardown and any offset restoration; `just hardware` runs packages sequentially so the tests do not contend with one another.

Contention exits non-zero (`run` and `reset`: 3), naming the host lock path, before any SMU or SMN access and without writing an event or preparing a carry. Failure to open or acquire the lock also prevents hardware access. A process exit releases the `flock`; on Linux, `/run` is a tmpfs as well, so a crash cannot leave a stale lock. The lock file is not removed on release. It is created with mode 0644 and opened read-only, so delegated hardware-test users can acquire an existing readable lock without write permission; acquisition does not replace the file or change its permissions.

The state-directory writer lock remains: it protects journal integrity, not the shared SMU mailbox. `status`, `cert`, `events` and `watch` take neither lock and never touch the SMU. The host lock coordinates togi processes only; other clients of `ryzen_smu` can bypass it.

Before a successful leftover-scope sweep, a journal write failure reports the error and leaves offsets unchanged: even emergency zeroing must not write a profile while an old workload may still be loading it. After the sweep succeeds, the usual journal-write emergency behavior applies, including a failure to record the successful sweep itself.

## Privileges

`run` and `reset` require root: they change core voltage and the boot entry, and create cgroup scopes. The read-only commands work for any user who can read the state directory.

Togi stays root. Backend workloads run as the unprivileged account named by `backend_user`, using that account's UID and primary GID with no inherited supplementary groups. The NixOS module declares the `togi-trial` system user and group and writes its name into the configuration. A missing account setting, nonexistent account, invalid UID or GID, UID 0 or primary GID 0 refuses preflight; workloads never fall back to root.

## Preflight

Hardware construction checks CPU family/model from `/proc/cpuinfo` and the driver codename from its sysfs metadata before reading any SMN register, including the slot-mapping fuses. An unsupported identity leaves the driver available for topology and preflight introspection but refuses every mailbox command and SMN access. CLI startup repeats this non-command validation and skips BIOS context reads when validation fails, then lets session preflight record the refusal and take the normal dead-end path (exit 15, including the watchdog check and saved-entry clear in a tuning boot). Session preflight establishes its own validation boundary before command-bearing checks; failures while writing earlier startup events must not emergency-zero the hardware.

Completed session preflight is the startup boundary for stale-scope containment and subsequent same-boot reconciliation, before `startSession` and the first offset write. An interrupted same-boot dead-end completion passes this same boundary before reconciling hardware and completing its recorded action; it starts no tuning.

`run` checks, each recorded as a `preflight.check` event:
1. In a tuning boot (`--tuning-boot`), a hardware watchdog is armed: at least one `/sys/class/watchdog/watchdog*/state` is `active`, with a readable, nonempty `identity` other than `Software Watchdog`. The software watchdog cannot recover a hardware freeze. This check runs before the other checks: it checks immediately, then polls every 1 second for at most 30 seconds, recording one final `preflight.check` named `watchdog`, not one event per poll. Normal/manual `run` without `--tuning-boot` neither requires nor waits for a watchdog.
2. Running as root.
3. The CPU is family `0x1A`, model `0x40`-`0x4F` (Granite Ridge desktop).
4. `ryzen_smu` is loaded and reports a matching codename.
5. Every core's offset reads back through the SMU.
6. Per-core access is supported only on full 8-core CCDs (`../prior-art.md`): each CCD fuse is read twice with distinct RSMU register reads interleaved, and the OS topology must contain eight cores per CCD. Any fused-off slot fails this check with the CCD, its fuse mask and a message that harvested CCDs are not yet supported. Indistinguishable RSMU probes, inconsistent fuse reads, or a live-core count mismatch also refuse all per-core access.
7. Both backends are configured and present. mprime's `bin/mprime` and both selected y-cruncher binaries must be regular executable files; otherwise preflight fails naming the file. y-cruncher selects the lexically first name matching `<two-digit ISA index>-<alphanumeric ISA> ~ <name>` in `lib/y-cruncher/Binaries` for the lowest ISA and the first matching `24-ZN5 ~ <name>` for Zen 5, ignoring unrelated files.
8. `backend_user` resolves to a non-root UID and primary GID.
9. `systemd-run` can create a scope confined to CPU 0 with those credentials.
10. The BIOS context matches the session, when resuming and all required checks passed.

Any failed check is a dead end. An unarmed watchdog after the bounded wait is dead end `preflight` (exit 15), with the tuning boot's usual saved-entry clear deferred until any required same-boot reconciliation succeeds; no SMU offset write or trial can happen before the watchdog check succeeds. When no same-boot reconciliation is required, a stop signal during the wait records `shutdown` and exits 0, rather than recording a watchdog failure.

## Same-boot resume

Before appending startup events, `run` compares the current boot ID with the boot IDs already recorded in the journal. A matching boot means an earlier process may have left offsets applied, even if its restoration was interrupted or its last event says the profile was restored. After successful preflight and stale-scope containment, and before finishing a pending dead end or continuing the session, it drains the journal's outstanding failure attribution, marks and backoffs without starting new tuning work, reads every core's actual offset and records `smu.readback`. Those actual readbacks establish this process's ownership of the inherited offsets.

It then restores the journal's mark-aware safe target using the same intent, write, readback and `profile.restored` sequence as exit restoration. Cores already at their safe targets need no writes. Repeated interruptions of these reads or restoration resume by reading every core again; a pending dead end cannot record `shutdown` before reconciliation succeeds. A failed preflight performs no reconciliation and leaves that completion pending.

Required same-boot reconciliation is bounded cleanup even after a stop signal. Cancellation does not short-circuit its startup preflight: the tuning boot still requires watchdog readiness and retains the 30-second watchdog deadline. Long kernel-log retry waits remain cancelable; cancellation during recovery still passes through this bounded preflight and restoration before a signal shutdown. An unarmed watchdog leaves the inherited offsets untouched and records no clean shutdown. After successful preflight and restoration, the stop signal still prevents starting tuning work.

A failed same-boot preflight records its checks and returns the preflight failure without completing a saved-entry action or recording `shutdown`. An existing pending dead end retains its original event, action and evidence; the failed preflight does not replace it. With no pending dead end, the new preflight dead end is recorded but its action remains pending. Once preflight and reconciliation succeed, the next run completes the original pending action, including any reboot request.

A different boot ID retains the firmware-reset assumption: no special reconciliation reads or writes occur. An interrupted dead end on a different boot completes its recorded action without preflight or tuning.

After every successful nonwriting preflight, on fresh start and resume in either run mode, `run` sweeps leftover `togi-trial-*.scope` units before the first SMU profile write, including any resume-time reconciliation writes. The sweep stops only concrete unit names with the exact `togi-trial-` prefix and `.scope` suffix; it never touches preflight scopes or names embedding that prefix. Recovered processes receive signals only through bounded `systemctl kill --kill-whom=all` calls on those exact scopes, never through a recovered PID or process-group number. Listing, bounded teardown and final unit/process verification share one 15 s deadline regardless of scope count. A `preflight.check` named `trial_scopes` records the result. If listing, teardown or verification fails, `containment` is a dead end; no profile write or trial follows.

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
| `backend_user` | not configured; module sets `"togi-trial"` | Existing account with non-root UID and primary GID; checked by hardware preflight |

Unknown keys and out-of-range values are errors. When `--config` is not given and no file exists at the default path, the defaults apply.

`durations.confirmation_trial_s` was removed: a file still setting it fails with `durations.confirmation_trial_s was removed in togi 0.5.0: confirmation no longer exists; delete the key`. Invalid evidence values report `evidence.miss = %g: must be within (0, 1)` (likewise rate); an evidence pair requiring over 1000 starts reports `evidence: miss %g and rate %g need more than 1000 starts per step`.

The number of starts for a full evidence step is `ceil(ln(evidence.miss) / log1p(-evidence.rate))`, five at defaults. Durations and workload identity are part of the trial class, so a change to either never turns passes of a different class into valid evidence.

The effective configuration is recorded in `config.loaded` at every start. Configuration that changes the meaning of existing evidence, such as trial durations, is allowed mid-session and takes effect from the next trial. The journal shows when it changed.

The TOML configuration has no version field. Renamed or removed options are handled in the NixOS module with `mkRenamedOptionModule` or `mkRemovedOptionModule` and noted in the changelog; changed configurable defaults are not a ruleset break.

## In-session run

`sudo togi run` on a normal boot. If the machine crashes, it comes back to the normal desktop. The next `sudo togi run` detects the crash from the journal, attributes it to the in-flight action, and continues. Nothing restarts togi automatically outside a tuning boot.

## Tuning boot

`services.togi.tuning.enable` adds a GRUB specialisation named `togi` and makes GRUB remember the last booted entry (`boot.loader.grub.default = "saved"`). Picking "togi" once in the menu starts unattended tuning. Every crash reboot returns to it until a dead end, `togi.service` reaching its restart limit, an orderly shutdown or reboot (unless `tuning.leaveOnShutdown` is off), or you, select a normal entry. The module requires GRUB with exactly one `boot.loader.grub.mirroredBoots` entry: the tuning boot supports a single GRUB environment. Other bootloaders are [#18](https://github.com/shgew/togi/issues/18).

Inside the specialisation:
- `systemd.defaultUnit = "multi-user.target"`, so no graphical session starts;
- `togi.service` runs `togi run --tuning-boot <grubenv>` as root, with `Restart=on-failure`, `RestartSec=60`, a start limit of 3 per 30 minutes, and `RestartPreventExitStatus` listing dead-end exit codes 10-18;
- `togi.service`, `togi-restart-limit.service` and `togi-leave-tuning-boot.service` require the mount holding `<grubenv>` with `RequiresMountsFor`, including when that mount has `noauto`;
- `togi.service` has `OnFailure=togi-restart-limit.service`. systemd starts that oneshot after every failure, since each automatic restart passes through the failed state; it does nothing unless `MONITOR_SERVICE_RESULT` is `start-limit-hit`, the result of a fourth start within 30 minutes. Then it runs `grub-editenv <grubenv> unset saved_entry` and, only if that succeeded, `systemctl reboot` into the normal system, whether or not `tuning.leaveOnShutdown` is on. Dead ends are never restarted, so they end with result `exit-code` and keep their own boot action and dashboard;
- with `tuning.leaveOnShutdown` (the default), `togi-leave-tuning-boot.service`, a oneshot unit ordered before `togi.service`, so it stops after it, whose `ExecStop` runs `grub-editenv <grubenv> unset saved_entry`. An orderly shutdown or reboot therefore hands the next boot to the newest normal generation; a panic, a watchdog reset or a power loss never runs it, so crash reboots stay in the tuning boot. The clear is logged to the system journal, not to togi's journal. The unit is not restarted when a configuration switch changes it;
- `togi-watch.service` runs `togi watch` on tty1 with `TERM=linux`, `Restart=always` and `Nice=-10`, in place of the tty1 login prompt. `togi.service` keeps the default niceness: its backends run as children of its scopes and would inherit it;
- `togi-console.service` follows `togi.service`'s log on tty3, in place of the tty3 login prompt; tty2 keeps its login;
- nothing else writes to tty1. The kernel command line is the system's, without its `console=ttyN` entries, plus `console=tty3`; it replaces kernel parameters defined for the specialisation alone, since a module cannot remove entries from a list option otherwise, so `/dev/console` output (early boot, systemd status, the emergency shell, panic text) goes to tty3; serial consoles stay. `console=` only picks the terminal behind `/dev/console`: the kernel prints its messages on the foreground terminal unless redirected, so `togi-kernel-log.service`, a oneshot ordered before the dashboard, runs `setlogcons 3` to send them to tty3 as well. Alt+F3 shows the boot log and kernel messages;
- `console.font` forced to `tuning.consoleFont`, by default `null`, the kernel's built-in font, so a system font without IBM437's block glyphs never reaches the dashboard;
- sysctls `kernel.panic=10`, `kernel.panic_on_oops=1`, `kernel.hardlockup_panic=1`, `kernel.softlockup_panic=1`;
- `boot.initrd.kernelModules = [ "sp5100_tco" ]`, loading the SP5100 TCO hardware watchdog driver in the initrd, before `togi.service` can start; this is tuning-only and does not change the normal boot's initrd module list;
- `systemd.settings.Manager.RuntimeWatchdogSec = "30s"`, so PID 1 arms and feeds the hardware watchdog to reset a frozen machine. The preflight gate waits for the kernel's active watchdog state rather than assuming that this setting or a loaded driver means it is already armed;
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

If a `run` starts after `deadend` but before `boot.saved_entry`, it completes the recorded action without starting a trial: after same-boot preflight and reconciliation, or immediately after a reboot, it clears GRUB's saved entry, appends `boot.saved_entry`, records `shutdown`, and exits with the same dead-end condition and evidence. A `clear_saved_entry_and_reboot` action still requests a reboot after a successful clear; a failed clear records the error and exits without rebooting. A `run` without `--tuning-boot` cannot clear the entry: it records `shutdown` and exits with the dead-end condition, leaving GRUB's saved entry as the operator last chose it. If `boot.saved_entry` exists but `shutdown` does not, the next `run` reconciles in the same boot, then records `shutdown` and exits without clearing again or tuning. An interrupted in-session `exit` action likewise reconciles in the same boot before recording `shutdown` and exiting. Once the dead end and shutdown are fully recorded, a later `run` starts normally and re-evaluates the conditions; a still-failing preflight stops before tuning.

## NixOS module

`nixosModules.default` from the flake:

| Option | Meaning |
|---|---|
| `services.togi.enable` | Install togi and load `ryzen_smu` (`hardware.cpu.amd.ryzen-smu.enable`, set with `mkDefault`, so a host that loads its own build can turn it off) |
| `services.togi.package` | Package to install and use for `togi.service` and `togi-watch.service`; defaults to the flake's togi package for the host platform |
| `services.togi.tuning.enable` | Add the tuning boot specialisation above |
| `services.togi.tuning.leaveOnShutdown` | Default `true`: an orderly shutdown or reboot of the tuning boot clears GRUB's saved entry, so the next boot is the normal system. `false` keeps the tuning boot selected until a dead end, `togi.service`'s restart limit, or you pick another entry |
| `services.togi.tuning.consoleFont` | The tuning boot's console font, as `console.font` takes it, whatever the system sets. Default `null`: the kernel's built-in font, 8x16 below 2560x1080 and Terminus 16x32 bold from there. Both give the 240x67 frame the dashboard is laid out for at 1080p and 4K, and both cover IBM437, whose block and box glyphs the dashboard draws with; a system font such as `Lat2-Terminus16` lacks `▀` and breaks the big digits. A font set here must cover IBM437 as well: Terminus' `ter-i` fonts, such as `"${pkgs.terminus_font}/share/consolefonts/ter-i32b.psf.gz"`, do, while `ter-v` and `Lat2-Terminus` fonts do not |
| `services.togi.settings` | Freeform attrset rendered to `/etc/togi/config.toml`; `backend_user` defaults to the module-declared `togi-trial` system user. An override must name an existing unprivileged account |
| `services.togi.backends.mprime.enable` | Set `settings.backends.mprime` to the nixpkgs `mprime` package (unfree) |
| `services.togi.backends.ycruncher.enable` | Set `settings.backends.ycruncher` to the nixpkgs `y-cruncher` package (unfree) |

The flake also exposes `packages.x86_64-linux.default` and a dev shell, and the same for `aarch64-darwin` for development, where the checks are `package`, `lint` and `fmt`, without the VM tests. The package builds only `cmd/togi` and runs the tests of every package, so the installed package contains only the `togi` executable. The release procedure and version bump rules are in [Releasing](../releasing.md).
