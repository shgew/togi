# Changelog

All notable changes to shycler are documented in this file. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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

[0.1.0]: https://github.com/shgew/shycler/releases/tag/v0.1.0

[0.2.0]: https://github.com/shgew/shycler/releases/tag/v0.2.0

[0.2.1]: https://github.com/shgew/shycler/releases/tag/v0.2.1

[#1]: https://github.com/shgew/shycler/issues/1
[#2]: https://github.com/shgew/shycler/issues/2
[#3]: https://github.com/shgew/shycler/issues/3
[#4]: https://github.com/shgew/shycler/issues/4
[#5]: https://github.com/shgew/shycler/issues/5
[#6]: https://github.com/shgew/shycler/issues/6
[#7]: https://github.com/shgew/shycler/issues/7
[#8]: https://github.com/shgew/shycler/issues/8
[#9]: https://github.com/shgew/shycler/issues/9
[#10]: https://github.com/shgew/shycler/issues/10
[#11]: https://github.com/shgew/shycler/issues/11
[#12]: https://github.com/shgew/shycler/issues/12
[#13]: https://github.com/shgew/shycler/issues/13
[#14]: https://github.com/shgew/shycler/issues/14
[#15]: https://github.com/shgew/shycler/issues/15
[#22]: https://github.com/shgew/shycler/issues/22
[#23]: https://github.com/shgew/shycler/issues/23
[#26]: https://github.com/shgew/shycler/issues/26
[#28]: https://github.com/shgew/shycler/issues/28
[#31]: https://github.com/shgew/shycler/issues/31
[#32]: https://github.com/shgew/shycler/issues/32
[#34]: https://github.com/shgew/shycler/issues/34
[#35]: https://github.com/shgew/shycler/issues/35
[#36]: https://github.com/shgew/shycler/issues/36
[#37]: https://github.com/shgew/shycler/issues/37
[#38]: https://github.com/shgew/shycler/issues/38
[#39]: https://github.com/shgew/shycler/issues/39
[#40]: https://github.com/shgew/shycler/pull/40
[#44]: https://github.com/shgew/shycler/pull/44
[#46]: https://github.com/shgew/shycler/pull/46
[#48]: https://github.com/shgew/shycler/pull/48
[#49]: https://github.com/shgew/shycler/pull/49
