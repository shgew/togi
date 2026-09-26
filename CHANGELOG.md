# Changelog

All notable changes to shycler are documented in this file. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Changed

- `shycler run` on a terminal shows the `watch` dashboard instead of one line per event, and prints the outcome when it stops; `--no-tui` keeps the lines ([#69]).

## [0.2.1] - 2026-09-26

### Added

- `shycler watch` shows the session as a dashboard that redraws every second: stage, running trial, one tile per core with its offset, failed mark and regainable or settled depth, and the latest events. Off a terminal it prints one frame and exits ([#65]).
- `services.shycler.tuning.consoleFont` sets the tuning boot's console font; by default the tuning boot uses the kernel's built-in font, whatever font the system sets, because the dashboard's digits need IBM437 block glyphs that fonts such as `Lat2-Terminus16` lack ([#65]).
- `candidate_edges.<core>` starts a core of a new session in confirmation at the given offset instead of searching, so a session started again after `reset --all` can skip re-finding known edges ([#66]).

### Changed

- The tuning boot shows `shycler watch` on tty1 instead of the service log, which moves to tty3 along with kernel messages and `/dev/console` output, so nothing else writes over the dashboard ([#65]).

## [0.2.0] - 2026-09-26

### Changed

- **BREAKING** Confirmation now runs every R1 and R2 workload, then R3, R4 and R5, at each candidate edge; a failure restarts the nine-trial set from the first R1 workload ([#58]).
- **BREAKING** Guard R7 runs CCD0, CCD1 and then all cores as separate resident trials with a shared workload instead of switching loads within one trial ([#58]).
- **BREAKING** Unattributed resident failures back off only their loaded cores (or every nonzero core when that narrower scope is at zero), instead of escalating later failures to every core ([#58]).
- **BREAKING** After each clean rotation, guard automatically regains one count per core with regainable depth and tests the changed profile; each step gets one retry and settles if suspect-backed-off from that step again, until `reset --core` ([#58]).
- **BREAKING** Bronze waits until every core is confirmed, no regainable depth remains and the current profile survives one clean rotation; settled depth alone does not block a tier ([#58]).
- `status` and `cert` now show REGAINABLE and SETTLED depth separately rather than total unproven depth ([#58]).
- `durations.guard_all_core_s` now requires at least 4 seconds so the separate R7 trials have time to run ([#58]).
- **BREAKING** Tuning ruleset and journal schema are now 2: archive a ruleset-1 session with `sudo shycler reset --all` before starting again. Setting `start_offsets.<core>` to an archived failed mark plus one can skip early search steps, but is a hint, not evidence ([#58]).

### Removed

- **BREAKING** Removed `shycler regain` and its isolated re-confirmation; guard regains eligible depth automatically after clean rotations ([#58]).
- **BREAKING** Removed the escalation window and its journal event; each unattributed failure is blamed by its own trial's loaded cores ([#58]).

## [0.1.0] - 2026-09-26

### Added

- Shutting down or rebooting the tuning boot on purpose clears GRUB's saved entry, so the next boot is the normal system; crash reboots still return to the tuning boot. `services.shycler.tuning.leaveOnShutdown = false` turns it off ([#53]).
- The flake provides the package, dev shell and checks on aarch64-darwin for development; the VM test and hardware runs stay Linux-only, and `shycler run` there exits with an error ([#45]).
- On resume, `run` finds decisions made by a known, since-fixed bug and names the affected cores; `status` shows the reset command, and in a terminal `run` offers to reset them. The first entry is the false failure at power-off fixed in [#16] ([#40]).
- Every session start and resume records the build's version, revision, ruleset and schema, and `run` refuses to resume a session written under a different ruleset or schema, naming the version that wrote it; in a tuning boot this is a dead end ([#38]).
- `status`, `cert` and `events` warn when a journal was written under a different ruleset, and refuse one with a different schema ([#38]).
- The run log and `shycler events` color failures and dead ends red, passed steps, confirmations and tiers green, backoffs yellow and inconclusive trials dim, on a terminal or in the system journal, where failures are logged at priority err; `NO_COLOR` turns it off ([#35]).
- `shycler --version` prints the version and the git revision of the build ([#34]).
- `sudo shycler run` tunes the real machine: offsets through `ryzen_smu` with a fuse-verified core-to-slot mapping, mprime and y-cruncher trials confined to their cores in systemd scopes, and machine checks read from the kernel log ([#13]).
- Preflight checks the CPU, `ryzen_smu`, SMU readback, the slot mapping, both backends and `systemd-run` before a hardware run ([#13]).
- `shycler run --tuning-boot <grubenv>` clears GRUB's saved entry at a dead end, and reboots into the normal system after a boot loop ([#13]).
- The NixOS module `nixosModules.default` (`services.shycler`) and the GRUB tuning boot entry "shycler" that tunes unattended ([#13]).
- `run` records a clean stop when its terminal closes (SIGHUP) ([#13]).
- The wordmark and tagline print above the usage when `shycler` runs without a command ([#11]).
- `shycler regain` queues one count of regain on cores with unproven depth, and `shycler reset` restarts one core's search or archives the whole session ([#9]).
- Durability tiers, `shycler status` for the session at a glance, and `shycler cert` for the certificate with the edges to enter in BIOS ([#8]).
- The guard: after every core is confirmed, all offsets are tested together in endless rotations through R1 to R7, backing off a core that fails and counting clean hours ([#7]).
- `shycler run --sim <seed>` runs a whole session on a simulated machine, and a run after a crash attributes it to the action in flight and continues ([#6]).
- A seeded simulator of a 16-core Zen 5 machine, with hidden per-core edges, random failures and crashes ([#5]).
- Per-core search, in steps of 5 and then 1 from the starting offset towards -50, and confirmation across regimes R1 to R5 at the candidate edge ([#4]).
- The journal: every action and decision is recorded crash-safely in `events.jsonl`, and `shycler events` prints it, filtered by core, kind, trial or time ([#3]).
- The `shycler` command, its TOML configuration, which rejects unknown keys and out-of-range values, and a Nix flake with the package and a dev shell ([#2]).

### Changed

- `shycler reset --all` also moves the trial work directories to `archive/<session-id>-trials/` ([#13]).
- Every command's `--help` gives a description and examples, lists its own flags before the global ones, and shows every flag in `--long` form ([#12]).
- `run` writes every core back to its baseline, or to its current offset where that is shallower, before stopping cleanly or at a dead end, instead of leaving the tested offsets applied until the next reboot ([#14]).
- A signal still interrupts the running trial, but `run` now records the decisions from that trial before stopping instead of leaving them to the next run ([#14]).

### Removed

- `shycler run --sim`: simulate from a source checkout with `just sim` (`go run ./tools/sim`) ([#46]).

### Fixed

- A trial interrupted by a signal now records how long it ran, instead of `after 0s` ([#15]).
- Powering off during a trial no longer records a failure on the target core: trial scopes no longer stop with the system before `run` does, so the trial ends interrupted ([#16]).
- A trial the machine crashed during now records how long it is known to have run, up to its last recorded progress, instead of `after 0s` ([#17]).
- Every journal event is fsynced, not only intents, so a crash no longer loses up to a minute of `events.jsonl`: the SMU write, readback and trial start before a crash stay recorded, and a crashed trial's duration runs to its last progress ([#54]).

[0.1.0]: https://code.marleb.org/shgew/shycler/releases/tag/v0.1.0

[0.2.0]: https://code.marleb.org/shgew/shycler/releases/tag/v0.2.0

[0.2.1]: https://code.marleb.org/shgew/shycler/releases/tag/v0.2.1

[#2]: https://code.marleb.org/shgew/shycler/pulls/2
[#3]: https://code.marleb.org/shgew/shycler/pulls/3
[#4]: https://code.marleb.org/shgew/shycler/pulls/4
[#5]: https://code.marleb.org/shgew/shycler/pulls/5
[#6]: https://code.marleb.org/shgew/shycler/pulls/6
[#7]: https://code.marleb.org/shgew/shycler/pulls/7
[#8]: https://code.marleb.org/shgew/shycler/pulls/8
[#9]: https://code.marleb.org/shgew/shycler/pulls/9
[#11]: https://code.marleb.org/shgew/shycler/pulls/11
[#12]: https://code.marleb.org/shgew/shycler/pulls/12
[#13]: https://code.marleb.org/shgew/shycler/pulls/13
[#14]: https://code.marleb.org/shgew/shycler/pulls/14
[#15]: https://code.marleb.org/shgew/shycler/pulls/15
[#16]: https://code.marleb.org/shgew/shycler/pulls/16
[#17]: https://code.marleb.org/shgew/shycler/pulls/17
[#34]: https://code.marleb.org/shgew/shycler/pulls/34
[#35]: https://code.marleb.org/shgew/shycler/pulls/35
[#38]: https://code.marleb.org/shgew/shycler/pulls/38
[#40]: https://code.marleb.org/shgew/shycler/pulls/40
[#45]: https://code.marleb.org/shgew/shycler/pulls/45
[#46]: https://code.marleb.org/shgew/shycler/pulls/46
[#53]: https://code.marleb.org/shgew/shycler/pulls/53
[#54]: https://code.marleb.org/shgew/shycler/pulls/54
[#58]: https://code.marleb.org/shgew/shycler/pulls/58
[#65]: https://code.marleb.org/shgew/shycler/pulls/65
[#66]: https://code.marleb.org/shgew/shycler/pulls/66
