# Changelog

All notable changes to togi are documented in this file. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- A ruleset, schema or evidence-epoch transition records eligible trial facts from earlier same-BIOS sessions as `trial.carried`/`failure.carried` and stamps `session.start` with the evidence epoch; newer evidence epochs are refused, older epochs retain failures but drop incompatible passes. Carry waits for a validated current BIOS context, respects every `reset --all` boundary even after an interrupted reset, preserves edge and failed-mark provenance across interrupted evidence-epoch transitions, resumes incomplete fact prefixes and re-checks copied failures against newly known defects in their original journals ([#281]).

### Changed

- **BREAKING** The next `togi run` archives a ruleset-6 session and starts a ruleset-7 session that carries trial facts from earlier same-BIOS sessions: candidate-edge checks, hunt masks, reruns and refinement checks can be answered by carried passes, carried failures count everywhere, and decisions cite their evidence consistently across replay and shutdown. Rotations still qualify only on live passes, and `status` counts only live passes as this session's exposure ([#282]).
- Failure-rate bounds in `status` state that their 95% claim assumes a constant failure rate on the tested workloads; the numbers and rounding are unchanged ([#237]).
- `run` archives older-ruleset or older-schema journals containing unknown event kinds and derives carry from known events; current-session writes still refuse unknown kinds, while `reset --all` permits them only for its non-appending different-schema archive ([#270]).

### Fixed

- A journal write failure followed by unconfirmed workload teardown reports both errors and withholds all offset restoration, including emergency zeroing, while the backend may still be running ([#279]).
- `status` derives the guard's Tctl peak and source from the same post-clock resident passes as clean hours, preserving the peak across shallow backoffs and clearing it when a qualifying failure restarts the clock ([#235]).
- Idle failures record a monotonicity warning when eligible all-core R6 passes in one class contradict the failed profile, citing those trial ends without changing the ensuing hunt ([#236]).
- Competing starts or resets can no longer alter an upgrade's archives or pending carry before acquiring the state-directory writer lock; the lock now covers carry preparation and the complete session or reset ([#245]).
- An upgrade interrupted before recording its carried evidence retains the original source through a subsequent upgrade, instead of replacing it with the incomplete intervening session ([#246]).
- Sessions started in the same second receive readable numeric suffixes when an archived journal or trial directory already uses the timestamp; carry traversal orders those suffixes numerically ([#247]).
- A confirmed defect reset survives interruption: resume completes only the missing resets tied to the recorded answer, without asking again or resetting a core twice ([#249]).
- Partial journal or state writes are rejected instead of committing incomplete records; after a journal write or fsync error, tuning stops and the next open rebuilds its sequence from surviving complete lines ([#251]).
- Preflight refuses full CCDs whose core IDs disagree with their modulo-eight slots before per-core access; if a previously tuned CCD is now refused, run `togi reset --core N` for each of its cores before tuning it with a supported topology ([#278]).

### Removed

- `togi cert` is gone; use `togi status`, which reports the same profile and qualified rotations and now shows each core's `SLOT` for matching BIOS controls ([#269]).

## [0.7.0] - 2026-10-02

### Changed

- **BREAKING** The next `togi run` archives a ruleset-5 session and starts a ruleset-6 session with carried candidate edges and eligible same-BIOS failed marks, but no joint marks or passed rotations. Hunts can start at the longer failed duration when sufficient valid short starts cover the failing profile and no short-or-shorter failure in that regime has occurred since reset ([#243]).
- Hunts can probe a repeatedly attributed masked core before binary parts when two same-class failures at adjacent offsets corroborate it; a passing singleton returns to the ordinary split without crediting an untested group ([#243]).
- Bronze and `run --rotations` can credit an earlier clean qualifying rotation after a later deepening when it ended with every core done, followed the latest reset, covered an equal or deeper profile, and no failure since reset contradicted it. Silver and Gold still require clean hours since the unchanged tier clock ([#243]).

## [0.6.1] - 2026-10-02

### Changed

- Compatibility diagnostics consistently name togi in version comparisons and recovery commands ([#227]).

### Fixed

- `events --kind` rejects unknown kind or group names and explicitly empty lists with a usage error and valid names on stderr, instead of silently returning an empty success ([#228]).

## [0.6.0] - 2026-10-01

### Changed

- **BREAKING** The next `togi run` archives a ruleset-4 session and starts a ruleset-5 session seeded with its candidate edges, and with its failed marks when the archived session recorded the same BIOS context; its joint marks stay behind. In ruleset 5 a hunt anchors on the newest qualifying profile raised to the failing profile, so the hunt after a refinement round tests only the cores that round deepened instead of every core from all-zero ([#220]).
- A joint mark backs a member off to the offset where its edge probe passed with the rest of the joint at its failing offsets, instead of moving another member one count to a profile no trial had passed; on the target machine that rule moved a member one count after each of six hunts and the resident start crashed again every time ([#221]).
- Hunt parts and complements count the passes and failures of earlier hunts recorded after the latest reset of any core, including when combining earlier passes with new starts, so a later hunt no longer reruns masks an earlier one already established or restores evidence discarded by a reset; on the target machine each of five hunts reran 14 such masks, about 2.3 hours each time ([#222]).

## [0.5.3] - 2026-10-01

### Added

- Every trial keeps fsynced per-second CPU temperatures, loaded-core frequencies and available package power in `samples.jsonl`; slow sample-file setup or writes do not delay load steps, cancellation or the deadline. Excess samples are dropped, and the runner waits for storage work before returning. After a crash, the journal reports the last sample's time, Tctl and frequency range alongside the last backend evidence, including when stronger failure evidence or an inconclusive reset cause decides the outcome. Sample-file creation failures retain the trial's measured duration ([#219]).

### Changed

- Inconclusive trials caused by malformed or short process-stat samples include the quoted content read, truncated to 256 bytes for diagnosis ([#218]).

### Fixed

- A rerun's journal cause names the newest queued failure of its class, without changing which trials run or how passes are counted ([#217]).
- The host lock is private by default, with explicit hardware-test group access instead of public-readable delegation. Legacy locks retain their inode during migration; quiesce old lock openers or reboot to revoke existing descriptors. Lock contention no longer restarts the tuning service or triggers restart-limit boot recovery ([#213]).
- Journal-derived diagnostics and fields render terminal and bidi controls as visible escapes in human event lines, status, certificates, archive notices and dashboards, preventing cursor manipulation and forged output while preserving readable Unicode and raw journal evidence ([#214]).
- Watched-output floods yield to cancellation and supervision deadlines; teardown shares one bounded drain across instances, preserving final computation-error evidence while unconfirmed output prevents a pass or further tuning ([#215]).

## [0.5.2] - 2026-09-30

### Fixed

- Crash recovery reads the reset reason from the immediate following system boot, including boots without togi, only when matching journal sequence identities and consecutive sequence numbers establish continuity. Missing boots, logs or sequence metadata leave the reason unknown instead of borrowing a later reset. If a session recovered crashes after intervening non-togi boots, run `togi reset --core N` for every tuned core; older journals cannot identify which reset reasons came from the wrong boot ([#209]).

## [0.5.1] - 2026-09-30

### Added

- `services.togi.package` selects the togi package installed and run by the NixOS module ([#190]).
- `backend_user` names the unprivileged account for backend workloads; the NixOS module declares the `togi-trial` system user and group and sets the key ([#204]).

### Changed

- Per-core offsets are refused on CCDs with fused-off slots: preflight names the CCD and fuse mask, and full 8-core CCDs remain supported ([#163]).
- `togi cert` labels the profile values `OFFSET` instead of `EDGE` and shows each core's CCD and slot to match the BIOS per-core Curve Optimizer controls ([#161]).
- Binaries built with `go build` report the seven-character Git revision and `-dirty` for modified checkouts, unless an explicit build revision is supplied; builds without VCS information still report `+dev` ([#183]).
- `--config` is accepted only by `run` and `reset`; read-only commands no longer advertise it and reject it with exit 2 ([#187]).
- The tuning boot now requires exactly one GRUB mirror, refusing configurations with several GRUB environments ([#189]).
- Kernel logs are now read from a persisted, boot-local cursor at every trial boundary and clean shutdown, including dead ends, covering profile writes and retry waits; interrupted setup recovers the successful target readback boundary, between-trial MCEs are recorded and shown without feeding trial decisions, and a lost read interval keeps an intersecting trial inconclusive below stronger evidence, including valid MCEs returned before a cursor metadata error. If a session may have tuned through kernel-log gaps, run `togi reset --core N` for every tuned core; older journals cannot identify the affected trials ([#196]).
- Read-only commands can inspect same-schema journals containing future event kinds, including their original lines in `events`; `run` and `reset` refuse them with the kind and build stamps instead of replaying incomplete facts. Builds through 0.5.0 still reject unknown kinds ([#165]).
- mprime and y-cruncher now run with the configured account's UID and primary GID and no inherited supplementary groups, with writable instance directories and inputs while togi and its surrounding state remain root-owned. Missing, invalid or root backend credentials refuse hardware preflight instead of falling back to root ([#204]).

### Fixed

- The tuning boot waits for an armed hardware watchdog before writing offsets or starting a workload, and loads its driver in the initrd. A freeze in the first seconds after boot can no longer bypass an unarmed watchdog; if none arms within 30 seconds, preflight stops without tuning ([#194]).
- A truncated machine-check record no longer prevents the next independent record's decoded bank type from naming its core; genuinely interleaved records remain unattributed. To discard decisions based on a previously unattributed record, run `togi reset --core N` for each affected core ([#186]).
- y-cruncher preflight rejects non-executable binaries and ignores unrelated files when selecting its lowest-ISA and Zen 5 binaries ([#162]).
- One-frame `togi watch` now prints journal read or compatibility errors on stderr and exits 1 while retaining the problem frame on stdout; a missing journal remains a successful `no session yet` frame ([#193]).
- The tuning service requires the mount holding GRUB's environment, including a separate `noauto` boot mount ([#189]).
- Trial outcome precedence now preserves backend failures and containment escapes when the runner also reports cleanup or cancellation errors, and preserves MCE evidence when kernel-log reading fails ([#167]).
- Trials with unreadable or malformed thread or CPU-usage samples are now inconclusive, even if later samples succeed, unless higher-precedence failure evidence survives. If a session may have tuned through sampling errors, run `togi reset --core N` for every tuned core; older journals cannot identify the affected trials ([#168]).
- Trials whose current boot is explicitly missing from the kernel log are now inconclusive unless higher-precedence failure evidence survives; valid empty results and recovery from vacuumed older boots are unchanged. If a session may have tuned through missing current-boot logs, run `togi reset --core N` for every tuned core; older journals cannot identify the affected trials ([#172]).
- Backend early exits and stalls are now recorded immediately as typed failure evidence, so recovery preserves the affected core if the machine crashes before the trial ends ([#173]).
- MCEs now record their boot-local monotonic time, so crash recovery cannot attribute a trial to a machine check from before its profile was applied; outside-window MCEs remain visible without becoming trial causes, and older MCE events retain their previous replay behavior ([#200]).
- `run`, `reset` and hardware tests now share `/run/lock/togi.lock` on Linux (`/tmp/togi.lock` for copied-journal reset on Darwin), preventing concurrent togi processes with different state directories from interleaving SMU access or carry preparation; a busy lock stops the command before hardware access or another event, and delegated hardware-test users can acquire an existing readable lock without write permission ([#170]).
- `run` now stops and joins workloads, restores offsets and closes the journal on ordinary errors and panics as well as clean exits; unconfirmed workload teardown prevents ordinary restoration and a false clean shutdown, ordinary trial-runner errors count toward backend retries and `no_evidence`, and passed-trial marker and retention failures warn visibly in the dashboard without interrupting tuning ([#176]).
- Stopping after a failed trial now records its pending attribution, marks and backoffs before restoring a safe profile, including with nonzero BIOS offsets, without starting more tuning work; an unattributed failure with every core at CO 0 retains its dead-end outcome and tuning-boot cleanup ([#177]).
- Failed `state.json` projection writes now warn and continue tuning instead of emergency zeroing; the journal remains authoritative, the next start rebuilds stale state, and intervening warnings do not hide the clean-stop restoration summary ([#197]).
- Hardware startup now validates CPU family/model and the `ryzen_smu` codename before any mailbox command or SMN access, including BIOS context and slot mapping; unsupported identities follow the recorded preflight refusal and tuning-boot cleanup, and journal failures before session validation stop without emergency SMU writes ([#198]).
- Same-boot restarts now read every core and restore the journal's mark-aware safe offsets after preflight, including when signaled to stop during crash recovery, before completing an interrupted dead end or resuming tuning; failed preflight preserves pending actions and defers saved-entry clearing until reconciliation, and repeated restoration interruptions cannot leave tuned offsets behind a clean shutdown ([#203]).
- Systemd commands and kernel-log reads now have deadlines, so a hung command fails cleanup or the log read instead of holding the session indefinitely ([#166]).
- Trial ends and partial-start rollbacks now share a 15-second teardown across all instances, kill every known scope, and stop at a containment dead end if cleanup cannot be confirmed, instead of hanging on inherited output pipes or continuing tuning; unconfirmed cleanup also prevents offset restoration and a false clean shutdown ([#174]).
- Fresh starts and resumes now sweep leftover trial scopes after preflight and before any profile write, signal recovered workloads only through their exact systemd scope, and stop at a containment dead end without writing offsets if no-unit/no-process cleanup cannot be verified ([#199]).
- Backend stdout, stderr and watched-file lines over 64 KiB now make the trial inconclusive and trigger bounded teardown, retaining a diagnostic prefix instead of allowing unfinished lines or large file appends to grow runner memory without bound ([#202]).
- Backend log files are opened before handing instance directories to the workload account, preventing another instance from redirecting privileged log writes through symlinks. Watched files reject symlinks and special files without blocking, application-owned directories remain traversable under restrictive umasks, and inaccessible external ancestors refuse launch without permission changes. Relative trial directories also resolve correctly when a scope starts ([#204]).

## [0.5.0] - 2026-09-29

### Added

- An unattributed failure is now hunted: masked trials keep the failed trial's load and workload, put some cores at their failing offsets and the rest at an anchor profile, and delta debugging finds the core or cores behind it. The result is a failed mark on one core or a joint mark over several; before a joint is marked, edge probes move each member shallower in turn until the combination passes, so the mark sits at the shallowest offsets seen to fail. These are recorded in new `hunt.start`, `hunt.mask`, `hunt.end`, `hunt.skipped` and `mark.joint` events ([#69]).
- Refinement moves every core that is not done toward the profile with the most total depth that reaches no mark, and may move a core shallower (`yield`) so others can go deeper; each round is a `refine.round` and checks the deepened cores with n starts per class. The firmware's preferred-core ranking, recorded as `host.ranking`, breaks ties ([#69]).
- `evidence.miss` and `evidence.rate` (defaults 0.05 and 0.5) set how many passing starts a step needs, 5 at the defaults, and `durations.start_s` (default 120) sets the length of one start ([#69]).
- Crashes are classified by the kernel's `Previous system reset reason` line, shown on `crash.detected`: a power-button reset during a trial is a crash failure, a power-button reset between trials is inconclusive, and so is power loss once the kernel has been seen to report reasons; a thermal trip is a new `thermal_trip` dead end with exit code 18 ([#69]).
- An unreadable kernel log is retried after 1, 5 and 30 minutes, and a backend that keeps failing to start waits 1, 5 and 30 minutes before its last three tries, recorded as `backend.retry`, before the `no_evidence` dead end; a missing backend binary stops at once ([#69]).
- A journal write failure sets every core to CO 0 without an intent, reads each back, reports on stderr and stops ([#69]).
- When `togi.service` keeps failing without a dead end, the tuning boot clears GRUB's saved entry and reboots into the normal system after the third failure within 30 minutes, through a new `togi-restart-limit.service`; earlier failures are retried after a minute ([#69]).
- Every event `togi run` writes records `mono_ms`, the boot-local monotonic time: the machine checks that belong to a trial, and the duration of a trial interrupted within one boot, no longer depend on the wall clock ([#69]).

### Changed

- **BREAKING** Ruleset 4: confirmation, suspect backoffs and automatic regain are removed. A core's candidate edge must pass n starts each of R1 and R2 before it leaves search; guard runs qualifying rotations of starts, and Bronze needs every core done, no depth left for refinement and a clean qualifying rotation since the profile last went deeper. A ruleset-3 session is archived and seeds a new one ([#69]).
- **BREAKING** `durations.confirmation_trial_s` is removed; a configuration file still setting it is refused with a message naming the key ([#69]).
- The default `guard.rotation` is `R7 R7 R7 R2 R2 R2 R6 R5 R1 R1 R1 R3 R4 R6`, which covers every R1, R2 and R7 workload; a custom rotation that misses part of that coverage keeps guarding but never earns Bronze, and `togi status` names what it misses ([#69]).
- Silver and Gold count clean hours since the tier clock, which restarts at a failure on a profile at least as deep as the current one rather than at every profile change ([#69]).
- A BIOS change no longer stops at a preflight dead end: `togi run` archives the session and starts a new one that carries each core's edge ([#69]).
- `togi status` shows the activity (search, hunt and mask, refinement round and checks, guard rotation and steps), a JOINT column, joint marks, the open hunt and round, and exposure per regime and workload; `togi cert` shows joint marks, done cores, and starts per workload; `togi watch` shows joint-mark and anchor offsets and marks done, hunted and masked cores ([#69]).
- `run --rotations N` stops after N clean qualifying rotations once every core is done and refinement can reach no more depth, and `reset --core N` also clears every joint mark that includes core N ([#69]).

## [0.4.1] - 2026-09-29

### Changed

- `togi run` no longer refuses a session written by an older ruleset or journal schema: it archives it and starts a new session in which each core starts confirmation at its deepest isolated pass and never runs at or past its shallowest attributed failure, recorded in a new `session.carried` event and shown by `togi status`. After a BIOS change only the edges carry; `reset --all` still starts over with nothing carried, and a session from a newer build is still refused ([how-to](https://github.com/shgew/togi/blob/main/docs/howto.md#7-after-a-breaking-update), [#65]).

## [0.4.0] - 2026-09-28

### Added

- togi is open source under the MIT license. Bug reports are welcome; [CONTRIBUTING.md](https://github.com/shgew/togi/blob/main/CONTRIBUTING.md) says what else is accepted before 1.0 ([#59]).

### Changed

- **BREAKING** Published as togi at `github.com/shgew/togi`, replacing the former installed names: the command, the flake input `github:shgew/togi`, the NixOS options `services.togi`, the `togi*.service` units, `/var/lib/togi`, `/etc/togi/config.toml`, the GRUB entry "NixOS - togi" and the `TOGI_*` variables. The former option names have no aliases. The journal format and tuning rules are unchanged ([#58]).

## [0.3.1] - 2026-09-27

### Fixed

- `reset --all` warns about stale candidate edges also when the archived session was written under another ruleset, as after a breaking update ([#50]).

## [0.3.0] - 2026-09-27

### Changed

- **BREAKING** Confirmation runs mprime AVX-512 first, then the other eight trials in their previous order, so the workload most likely to fail no longer waits behind four passes; a ruleset-2 session must be archived with `togi reset --all` ([#47]).

## [0.2.2] - 2026-09-27

### Added

- `reset --all` warns about each configured candidate edge at or deeper than that core's failed mark in the session it archives ([#48]).

### Changed

- `status` labels its Tctl line as the current profile's maximum ([#44]).
- A trial closed after a crash or an interrupted run says how long after its start the last evidence was recorded, instead of a duration that read as measured ([#46]).
- `togi run` on a terminal shows the `watch` dashboard instead of one line per event, and prints the outcome when it stops, including the restored offsets and the reason after Ctrl-C; `--no-tui` keeps the lines ([#39]).
- togi moved to GitHub: the flake is now `github:shgew/togi`, and the issue and pull request numbers in this changelog, the docs and defect messages refer to `github.com/shgew/togi` ([#40]).
- The default guard rotation runs R2, R7, R6 and R5 before R1, R3 and R4, so regimes that failed in guard or were never reached before a failure restarted the rotation run first; a rotation already open keeps its order ([#49]).

## [0.2.1] - 2026-09-26

### Added

- `togi watch` shows the session as a dashboard that redraws every second: stage, running trial, one tile per core with its offset, failed mark and regainable or settled depth, and the latest events. Off a terminal it prints one frame and exits ([#37]).
- `services.togi.tuning.consoleFont` sets the tuning boot's console font; by default the tuning boot uses the kernel's built-in font, whatever font the system sets, because the dashboard's digits need IBM437 block glyphs that fonts such as `Lat2-Terminus16` lack ([#37]).
- `candidate_edges.<core>` starts a core of a new session in confirmation at the given offset instead of searching, so a session started again after `reset --all` can skip re-finding known edges ([#38]).

### Changed

- The tuning boot shows `togi watch` on tty1 instead of the service log, which moves to tty3 along with kernel messages and `/dev/console` output, so nothing else writes over the dashboard ([#37]).

## [0.2.0] - 2026-09-26

### Changed

- **BREAKING** Confirmation now runs every R1 and R2 workload, then R3, R4 and R5, at each candidate edge; a failure restarts the nine-trial set from the first R1 workload ([#36]).
- **BREAKING** Guard R7 runs CCD0, CCD1 and then all cores as separate resident trials with a shared workload instead of switching loads within one trial ([#36]).
- **BREAKING** Unattributed resident failures back off only their loaded cores (or every nonzero core when that narrower scope is at zero), instead of escalating later failures to every core ([#36]).
- **BREAKING** After each clean rotation, guard automatically regains one count per core with regainable depth and tests the changed profile; each step gets one retry and settles if suspect-backed-off from that step again, until `reset --core` ([#36]).
- **BREAKING** Bronze waits until every core is confirmed, no regainable depth remains and the current profile survives one clean rotation; settled depth alone does not block a tier ([#36]).
- `status` and `cert` now show REGAINABLE and SETTLED depth separately rather than total unproven depth ([#36]).
- `durations.guard_all_core_s` now requires at least 4 seconds so the separate R7 trials have time to run ([#36]).
- **BREAKING** Tuning ruleset and journal schema are now 2: archive a ruleset-1 session with `sudo togi reset --all` before starting again. Setting `start_offsets.<core>` to an archived failed mark plus one can skip early search steps, but is a hint, not evidence ([#36]).

### Removed

- **BREAKING** Removed `togi regain` and its isolated re-confirmation; guard regains eligible depth automatically after clean rotations ([#36]).
- **BREAKING** Removed the escalation window and its journal event; each unattributed failure is blamed by its own trial's loaded cores ([#36]).

## [0.1.0] - 2026-09-26

### Added

- Shutting down or rebooting the tuning boot on purpose clears GRUB's saved entry, so the next boot is the normal system; crash reboots still return to the tuning boot. `services.togi.tuning.leaveOnShutdown = false` turns it off ([#34]).
- The flake provides the package, dev shell and checks on aarch64-darwin for development; the VM test and hardware runs stay Linux-only, and `togi run` there exits with an error ([#31]).
- On resume, `run` finds decisions made by a known, since-fixed bug and names the affected cores; `status` shows the reset command, and in a terminal `run` offers to reset them. The first entry is the false failure at power-off fixed in [#14] ([#28]).
- Every session start and resume records the build's version, revision, ruleset and schema, and `run` refuses to resume a session written under a different ruleset or schema, naming the version that wrote it; in a tuning boot this is a dead end ([#26]).
- `status`, `cert` and `events` warn when a journal was written under a different ruleset, and refuse one with a different schema ([#26]).
- The run log and `togi events` color failures and dead ends red, passed steps, confirmations and tiers green, backoffs yellow and inconclusive trials dim, on a terminal or in the system journal, where failures are logged at priority err; `NO_COLOR` turns it off ([#23]).
- `togi --version` prints the version and the git revision of the build ([#22]).
- `sudo togi run` tunes the real machine: offsets through `ryzen_smu` with a fuse-verified core-to-slot mapping, mprime and y-cruncher trials confined to their cores in systemd scopes, and machine checks read from the kernel log ([#11]).
- Preflight checks the CPU, `ryzen_smu`, SMU readback, the slot mapping, both backends and `systemd-run` before a hardware run ([#11]).
- `togi run --tuning-boot <grubenv>` clears GRUB's saved entry at a dead end, and reboots into the normal system after a boot loop ([#11]).
- The NixOS module `nixosModules.default` (`services.togi`) and the GRUB tuning boot entry "togi" that tunes unattended ([#11]).
- `run` records a clean stop when its terminal closes (SIGHUP) ([#11]).
- The wordmark and tagline print above the usage when `togi` runs without a command ([#9]).
- `togi regain` queues one count of regain on cores with unproven depth, and `togi reset` restarts one core's search or archives the whole session ([#8]).
- Durability tiers, `togi status` for the session at a glance, and `togi cert` for the certificate with the edges to enter in BIOS ([#7]).
- The guard: after every core is confirmed, all offsets are tested together in endless rotations through R1 to R7, backing off a core that fails and counting clean hours ([#6]).
- `togi run --sim <seed>` runs a whole session on a simulated machine, and a run after a crash attributes it to the action in flight and continues ([#5]).
- A seeded simulator of a 16-core Zen 5 machine, with hidden per-core edges, random failures and crashes ([#4]).
- Per-core search, in steps of 5 and then 1 from the starting offset towards -50, and confirmation across regimes R1 to R5 at the candidate edge ([#3]).
- The journal: every action and decision is recorded crash-safely in `events.jsonl`, and `togi events` prints it, filtered by core, kind, trial or time ([#2]).
- The `togi` command, its TOML configuration, which rejects unknown keys and out-of-range values, and a Nix flake with the package and a dev shell ([#1]).

### Changed

- `togi reset --all` also moves the trial work directories to `archive/<session-id>-trials/` ([#11]).
- Every command's `--help` gives a description and examples, lists its own flags before the global ones, and shows every flag in `--long` form ([#10]).
- `run` writes every core back to its baseline, or to its current offset where that is shallower, before stopping cleanly or at a dead end, instead of leaving the tested offsets applied until the next reboot ([#12]).
- A signal still interrupts the running trial, but `run` now records the decisions from that trial before stopping instead of leaving them to the next run ([#12]).

### Removed

- `togi run --sim`: simulate from a source checkout with `just sim` (`go run ./tools/sim`) ([#32]).

### Fixed

- A trial interrupted by a signal now records how long it ran, instead of `after 0s` ([#13]).
- Powering off during a trial no longer records a failure on the target core: trial scopes no longer stop with the system before `run` does, so the trial ends interrupted ([#14]).
- A trial the machine crashed during now records how long it is known to have run, up to its last recorded progress, instead of `after 0s` ([#15]).
- Every journal event is fsynced, not only intents, so a crash no longer loses up to a minute of `events.jsonl`: the SMU write, readback and trial start before a crash stay recorded, and a crashed trial's duration runs to its last progress ([#35]).

[0.1.0]: https://github.com/shgew/togi/releases/tag/v0.1.0

[0.2.0]: https://github.com/shgew/togi/releases/tag/v0.2.0

[0.2.1]: https://github.com/shgew/togi/releases/tag/v0.2.1

[0.2.2]: https://github.com/shgew/togi/releases/tag/v0.2.2

[0.3.0]: https://github.com/shgew/togi/releases/tag/v0.3.0

[0.3.1]: https://github.com/shgew/togi/releases/tag/v0.3.1

[0.4.0]: https://github.com/shgew/togi/releases/tag/v0.4.0

[0.4.1]: https://github.com/shgew/togi/releases/tag/v0.4.1

[0.5.0]: https://github.com/shgew/togi/releases/tag/v0.5.0

[0.5.1]: https://github.com/shgew/togi/releases/tag/v0.5.1

[0.5.2]: https://github.com/shgew/togi/releases/tag/v0.5.2

[0.5.3]: https://github.com/shgew/togi/releases/tag/v0.5.3

[0.6.0]: https://github.com/shgew/togi/releases/tag/v0.6.0

[0.6.1]: https://github.com/shgew/togi/releases/tag/v0.6.1

[0.7.0]: https://github.com/shgew/togi/releases/tag/v0.7.0

[#1]: https://github.com/shgew/togi/issues/1
[#2]: https://github.com/shgew/togi/issues/2
[#3]: https://github.com/shgew/togi/issues/3
[#4]: https://github.com/shgew/togi/issues/4
[#5]: https://github.com/shgew/togi/issues/5
[#6]: https://github.com/shgew/togi/issues/6
[#7]: https://github.com/shgew/togi/issues/7
[#8]: https://github.com/shgew/togi/issues/8
[#9]: https://github.com/shgew/togi/issues/9
[#10]: https://github.com/shgew/togi/issues/10
[#11]: https://github.com/shgew/togi/issues/11
[#12]: https://github.com/shgew/togi/issues/12
[#13]: https://github.com/shgew/togi/issues/13
[#14]: https://github.com/shgew/togi/issues/14
[#15]: https://github.com/shgew/togi/issues/15
[#22]: https://github.com/shgew/togi/issues/22
[#23]: https://github.com/shgew/togi/issues/23
[#26]: https://github.com/shgew/togi/issues/26
[#28]: https://github.com/shgew/togi/issues/28
[#31]: https://github.com/shgew/togi/issues/31
[#32]: https://github.com/shgew/togi/issues/32
[#34]: https://github.com/shgew/togi/issues/34
[#35]: https://github.com/shgew/togi/issues/35
[#36]: https://github.com/shgew/togi/issues/36
[#37]: https://github.com/shgew/togi/issues/37
[#38]: https://github.com/shgew/togi/issues/38
[#39]: https://github.com/shgew/togi/issues/39
[#40]: https://github.com/shgew/togi/issues/40
[#44]: https://github.com/shgew/togi/issues/44
[#46]: https://github.com/shgew/togi/issues/46
[#47]: https://github.com/shgew/togi/issues/47
[#48]: https://github.com/shgew/togi/issues/48
[#49]: https://github.com/shgew/togi/issues/49
[#50]: https://github.com/shgew/togi/issues/50
[#58]: https://github.com/shgew/togi/pull/58
[#59]: https://github.com/shgew/togi/pull/59
[#65]: https://github.com/shgew/togi/pull/65
[#69]: https://github.com/shgew/togi/pull/69
[#163]: https://github.com/shgew/togi/pull/163
[#194]: https://github.com/shgew/togi/pull/194
[#186]: https://github.com/shgew/togi/pull/186
[#161]: https://github.com/shgew/togi/pull/161
[#162]: https://github.com/shgew/togi/pull/162
[#183]: https://github.com/shgew/togi/pull/183
[#187]: https://github.com/shgew/togi/pull/187
[#193]: https://github.com/shgew/togi/pull/193
[#189]: https://github.com/shgew/togi/pull/189
[#190]: https://github.com/shgew/togi/pull/190
[#167]: https://github.com/shgew/togi/pull/167
[#168]: https://github.com/shgew/togi/pull/168
[#172]: https://github.com/shgew/togi/pull/172
[#173]: https://github.com/shgew/togi/pull/173
[#196]: https://github.com/shgew/togi/pull/196
[#200]: https://github.com/shgew/togi/pull/200
[#165]: https://github.com/shgew/togi/pull/165
[#170]: https://github.com/shgew/togi/pull/170
[#176]: https://github.com/shgew/togi/pull/176
[#177]: https://github.com/shgew/togi/pull/177
[#197]: https://github.com/shgew/togi/pull/197
[#198]: https://github.com/shgew/togi/pull/198
[#203]: https://github.com/shgew/togi/pull/203
[#166]: https://github.com/shgew/togi/pull/166
[#174]: https://github.com/shgew/togi/pull/174
[#199]: https://github.com/shgew/togi/pull/199
[#202]: https://github.com/shgew/togi/pull/202
[#204]: https://github.com/shgew/togi/pull/204
[#209]: https://github.com/shgew/togi/pull/209
[#213]: https://github.com/shgew/togi/pull/213
[#214]: https://github.com/shgew/togi/pull/214
[#215]: https://github.com/shgew/togi/pull/215
[#217]: https://github.com/shgew/togi/pull/217
[#218]: https://github.com/shgew/togi/pull/218
[#219]: https://github.com/shgew/togi/pull/219
[#220]: https://github.com/shgew/togi/pull/220
[#221]: https://github.com/shgew/togi/pull/221
[#222]: https://github.com/shgew/togi/pull/222
[#227]: https://github.com/shgew/togi/pull/227
[#228]: https://github.com/shgew/togi/pull/228
[#235]: https://github.com/shgew/togi/pull/235
[#236]: https://github.com/shgew/togi/pull/236
[#237]: https://github.com/shgew/togi/pull/237
[#243]: https://github.com/shgew/togi/pull/243
[#245]: https://github.com/shgew/togi/pull/245
[#246]: https://github.com/shgew/togi/pull/246
[#247]: https://github.com/shgew/togi/pull/247
[#249]: https://github.com/shgew/togi/pull/249
[#251]: https://github.com/shgew/togi/pull/251
[#269]: https://github.com/shgew/togi/pull/269
[#270]: https://github.com/shgew/togi/pull/270
[#278]: https://github.com/shgew/togi/pull/278
[#279]: https://github.com/shgew/togi/pull/279
[#281]: https://github.com/shgew/togi/pull/281
[#282]: https://github.com/shgew/togi/pull/282
