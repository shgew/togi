# Changelog

All notable changes to togi, called shycler up to 0.3.1, are documented in this file. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- An unattributed failure is now hunted: masked trials keep the failed trial's load and workload, put some cores at their failing offsets and the rest at an anchor profile, and delta debugging finds the core or cores behind it. The result is a failed mark on one core or a joint mark over several, recorded in new `hunt.start`, `hunt.mask`, `hunt.end`, `hunt.skipped` and `mark.joint` events ([#69]).
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

- **BREAKING** shycler is now togi, at `github.com/shgew/togi`: the command, the flake input `github:shgew/togi`, the NixOS options `services.togi`, the `togi*.service` units, `/var/lib/togi`, `/etc/togi/config.toml`, the GRUB entry "NixOS - togi" and the `TOGI_*` variables. The old option names have no alias. The journal format and tuning rules are unchanged: after clearing any pending tuning boot, renaming the options and running `sudo mv /var/lib/shycler /var/lib/togi`, the session continues ([how-to](https://github.com/shgew/togi/blob/main/docs/howto.md#8-moving-from-shycler), [#58]).

## [0.3.1] - 2026-09-27

### Fixed

- `reset --all` warns about stale candidate edges also when the archived session was written under another ruleset, as after a breaking update ([#50]).

## [0.3.0] - 2026-09-27

### Changed

- **BREAKING** Confirmation runs mprime AVX-512 first, then the other eight trials in their previous order, so the workload most likely to fail no longer waits behind four passes; a ruleset-2 session must be archived with `shycler reset --all` ([#47]).

## [0.2.2] - 2026-09-27

### Added

- `reset --all` warns about each configured candidate edge at or deeper than that core's failed mark in the session it archives ([#48]).

### Changed

- `status` labels its Tctl line as the current profile's maximum ([#44]).
- A trial closed after a crash or an interrupted run says how long after its start the last evidence was recorded, instead of a duration that read as measured ([#46]).
- `shycler run` on a terminal shows the `watch` dashboard instead of one line per event, and prints the outcome when it stops, including the restored offsets and the reason after Ctrl-C; `--no-tui` keeps the lines ([#39]).
- shycler moved to GitHub: the flake is now `github:shgew/shycler`, and the issue and pull request numbers in this changelog, the docs and defect messages refer to `github.com/shgew/shycler` ([#40]).
- The default guard rotation runs R2, R7, R6 and R5 before R1, R3 and R4, so regimes that failed in guard or were never reached before a failure restarted the rotation run first; a rotation already open keeps its order ([#49]).

## [0.2.1] - 2026-09-26

### Added

- `shycler watch` shows the session as a dashboard that redraws every second: stage, running trial, one tile per core with its offset, failed mark and regainable or settled depth, and the latest events. Off a terminal it prints one frame and exits ([#37]).
- `services.shycler.tuning.consoleFont` sets the tuning boot's console font; by default the tuning boot uses the kernel's built-in font, whatever font the system sets, because the dashboard's digits need IBM437 block glyphs that fonts such as `Lat2-Terminus16` lack ([#37]).
- `candidate_edges.<core>` starts a core of a new session in confirmation at the given offset instead of searching, so a session started again after `reset --all` can skip re-finding known edges ([#38]).

### Changed

- The tuning boot shows `shycler watch` on tty1 instead of the service log, which moves to tty3 along with kernel messages and `/dev/console` output, so nothing else writes over the dashboard ([#37]).

## [0.2.0] - 2026-09-26

### Changed

- **BREAKING** Confirmation now runs every R1 and R2 workload, then R3, R4 and R5, at each candidate edge; a failure restarts the nine-trial set from the first R1 workload ([#36]).
- **BREAKING** Guard R7 runs CCD0, CCD1 and then all cores as separate resident trials with a shared workload instead of switching loads within one trial ([#36]).
- **BREAKING** Unattributed resident failures back off only their loaded cores (or every nonzero core when that narrower scope is at zero), instead of escalating later failures to every core ([#36]).
- **BREAKING** After each clean rotation, guard automatically regains one count per core with regainable depth and tests the changed profile; each step gets one retry and settles if suspect-backed-off from that step again, until `reset --core` ([#36]).
- **BREAKING** Bronze waits until every core is confirmed, no regainable depth remains and the current profile survives one clean rotation; settled depth alone does not block a tier ([#36]).
- `status` and `cert` now show REGAINABLE and SETTLED depth separately rather than total unproven depth ([#36]).
- `durations.guard_all_core_s` now requires at least 4 seconds so the separate R7 trials have time to run ([#36]).
- **BREAKING** Tuning ruleset and journal schema are now 2: archive a ruleset-1 session with `sudo shycler reset --all` before starting again. Setting `start_offsets.<core>` to an archived failed mark plus one can skip early search steps, but is a hint, not evidence ([#36]).

### Removed

- **BREAKING** Removed `shycler regain` and its isolated re-confirmation; guard regains eligible depth automatically after clean rotations ([#36]).
- **BREAKING** Removed the escalation window and its journal event; each unattributed failure is blamed by its own trial's loaded cores ([#36]).

## [0.1.0] - 2026-09-26

### Added

- Shutting down or rebooting the tuning boot on purpose clears GRUB's saved entry, so the next boot is the normal system; crash reboots still return to the tuning boot. `services.shycler.tuning.leaveOnShutdown = false` turns it off ([#34]).
- The flake provides the package, dev shell and checks on aarch64-darwin for development; the VM test and hardware runs stay Linux-only, and `shycler run` there exits with an error ([#31]).
- On resume, `run` finds decisions made by a known, since-fixed bug and names the affected cores; `status` shows the reset command, and in a terminal `run` offers to reset them. The first entry is the false failure at power-off fixed in [#14] ([#28]).
- Every session start and resume records the build's version, revision, ruleset and schema, and `run` refuses to resume a session written under a different ruleset or schema, naming the version that wrote it; in a tuning boot this is a dead end ([#26]).
- `status`, `cert` and `events` warn when a journal was written under a different ruleset, and refuse one with a different schema ([#26]).
- The run log and `shycler events` color failures and dead ends red, passed steps, confirmations and tiers green, backoffs yellow and inconclusive trials dim, on a terminal or in the system journal, where failures are logged at priority err; `NO_COLOR` turns it off ([#23]).
- `shycler --version` prints the version and the git revision of the build ([#22]).
- `sudo shycler run` tunes the real machine: offsets through `ryzen_smu` with a fuse-verified core-to-slot mapping, mprime and y-cruncher trials confined to their cores in systemd scopes, and machine checks read from the kernel log ([#11]).
- Preflight checks the CPU, `ryzen_smu`, SMU readback, the slot mapping, both backends and `systemd-run` before a hardware run ([#11]).
- `shycler run --tuning-boot <grubenv>` clears GRUB's saved entry at a dead end, and reboots into the normal system after a boot loop ([#11]).
- The NixOS module `nixosModules.default` (`services.shycler`) and the GRUB tuning boot entry "shycler" that tunes unattended ([#11]).
- `run` records a clean stop when its terminal closes (SIGHUP) ([#11]).
- The wordmark and tagline print above the usage when `shycler` runs without a command ([#9]).
- `shycler regain` queues one count of regain on cores with unproven depth, and `shycler reset` restarts one core's search or archives the whole session ([#8]).
- Durability tiers, `shycler status` for the session at a glance, and `shycler cert` for the certificate with the edges to enter in BIOS ([#7]).
- The guard: after every core is confirmed, all offsets are tested together in endless rotations through R1 to R7, backing off a core that fails and counting clean hours ([#6]).
- `shycler run --sim <seed>` runs a whole session on a simulated machine, and a run after a crash attributes it to the action in flight and continues ([#5]).
- A seeded simulator of a 16-core Zen 5 machine, with hidden per-core edges, random failures and crashes ([#4]).
- Per-core search, in steps of 5 and then 1 from the starting offset towards -50, and confirmation across regimes R1 to R5 at the candidate edge ([#3]).
- The journal: every action and decision is recorded crash-safely in `events.jsonl`, and `shycler events` prints it, filtered by core, kind, trial or time ([#2]).
- The `shycler` command, its TOML configuration, which rejects unknown keys and out-of-range values, and a Nix flake with the package and a dev shell ([#1]).

### Changed

- `shycler reset --all` also moves the trial work directories to `archive/<session-id>-trials/` ([#11]).
- Every command's `--help` gives a description and examples, lists its own flags before the global ones, and shows every flag in `--long` form ([#10]).
- `run` writes every core back to its baseline, or to its current offset where that is shallower, before stopping cleanly or at a dead end, instead of leaving the tested offsets applied until the next reboot ([#12]).
- A signal still interrupts the running trial, but `run` now records the decisions from that trial before stopping instead of leaving them to the next run ([#12]).

### Removed

- `shycler run --sim`: simulate from a source checkout with `just sim` (`go run ./tools/sim`) ([#32]).

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
