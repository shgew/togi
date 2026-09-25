# Changelog

All notable changes to shycler are documented in this file. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

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

### Fixed

- A trial interrupted by a signal now records how long it ran, instead of `after 0s` ([#15]).
- Powering off during a trial no longer records a failure on the target core: trial scopes no longer stop with the system before `run` does, so the trial ends interrupted ([#16]).
- A trial the machine crashed during now records how long it is known to have run, up to its last recorded progress, instead of `after 0s` ([#17]).

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
